# opentelemetry-traceable

Dynamic, per-function tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Annotate any `fn`/`async fn` with `#[traceable]`, then enable or disable tracing for it at
runtime — individually, in bulk, or as an arbitrary subset of tens of thousands of functions —
without recompiling.

## Quick start

Annotate the functions you might one day want traced:

```rust
use opentelemetry_traceable::traceable;

#[traceable]
fn process() { /* ... */ }

#[traceable(name = "kafka.fetch")]
async fn fetch() { /* ... */ }

#[traceable(fields("component" = "proxy", "request_id" = id.clone()))]
async fn handle(id: String) { /* ... */ }
```

Nothing traces yet. An **`Instrumentation`** is the only way to turn tracing on — there is no
global or default instrumentation, and no process-wide tracer. Each `Instrumentation` brings its
own `Tracer`, holds its own enabled subset, and builds its own isolated span hierarchy.

Use the re-exported opentelemetry modules to access functionality such as `sdk` and `otlp`:

```rust
use opentelemetry_traceable::opentelemetry::trace::TracerProvider as _;
use opentelemetry_traceable::instrumentation::Instrumentation;
use opentelemetry_traceable::opentelemetry_otlp::{self, WithExportConfig};
use opentelemetry_traceable::opentelemetry_sdk;

let exporter = opentelemetry_otlp::SpanExporterBuilder::default()
    .with_http()
    .with_endpoint("http://localhost:4317")
    .build()
    .unwrap();
let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
    .with_simple_exporter(exporter)
    .build();

let instr = Instrumentation::builder()
    .name("checkout-debug")                     // diagnostic only
    .tracer(provider.tracer("checkout-debug"))  // any opentelemetry Tracer; required
    .build()?;                                  // Err(BuildError) if no slot is free

instr.set_enabled(&["my_crate::checkout::*"])?; // keys and/or globs — see "Selecting functions"
```

`opentelemetry` is re-exported as `opentelemetry_traceable::opentelemetry` rather than being a
dependency you add yourself: `#[traceable]`'s expansion and `Instrumentation::tracer`/`Context`/
`KeyValue` all need to resolve to the _exact same_ `opentelemetry` this crate was built against,
and re-exporting it makes that structurally guaranteed instead of something you have to keep
your own `Cargo.toml` in sync with. `linkme` gets the same treatment, hidden, since the macro is
its only caller. Whatever `Tracer` implementation you build `provider` from (`opentelemetry_sdk`
above, or another backend) remains your own dependency — this crate has no opinion on it.

Dropping `instr` stops new spans for it and releases its slot for reuse; spans already in flight
finish and export normally.

Every `#[traceable]` function is disabled for every instrumentation by default. When no
instrumentation is tracing a function, a call costs a single atomic load — no span, no
`opentelemetry::Context` work at all. When at least one is, the macro builds one child span per
active instrumentation and attaches a single context for the wrapped call (attach/detach for sync,
`FutureExt::with_context` across `.await` for async), so parent/child nesting is automatic,
including across `.await` points.

An instrumentation is **in-process** by default: its spans nest under each other and nowhere else.
Add `.distributed()` to the builder for one that joins an incoming `traceparent` and is carried
outbound by a propagator — see
[In-process and distributed instrumentations](#in-process-and-distributed-instrumentations).

Disabling a function only skips _its own_ span — an enabled function still nests under the nearest
recording ancestor span **of the same instrumentation**, whether or not everything in between is
enabled.

A function's **registry key** — the string it's listed under, and the thing you configure by —
defaults to `module_path!() + "::" + fn_name`, or the `name` argument if given. It's the whole
identity of a trace site: there is no id or index, and nothing is addressed by position. Note it
isn't qualified by a surrounding `impl` type — two methods with the same name in the same module
share a key, and therefore toggle together, unless `name` disambiguates them.

The macro takes only `name` and `fields(...)`. There is deliberately no `tracer` argument: a call
site never names or looks up a tracer, because each instrumentation supplies its own.

## Configuring an instrumentation

An instrumentation is configured by **naming the functions it should trace** — their registry keys,
or `*` globs over them. That's the whole interface; the way DTrace names probes, over a probe set
that happens to be declared at compile time.

```rust
instr.set_enabled(&["my_crate::checkout"])?;   // replace the whole enabled set
instr.enable(&["my_crate::db::*"])?;           // additive
instr.disable(&["my_crate::db::*"])?;          // subtractive
instr.enable_all();                            // every registered function
instr.disable_all();                           // none

instr.enabled_names();                         // -> impl Iterator<Item = &'static str>, read-only

opentelemetry_traceable::registry::keys();     // -> every registered key, regardless of instrumentation
```

All three setters take a slice of selectors and return `Result<Selection, UnknownKeys>` — see
[Selecting functions by key](#selecting-functions-by-key). `enabled_names` is introspection only,
and sorted. `keys()` lives on the registry rather than on a handle, because the set of linked
`#[traceable]` functions is a property of the binary, not of any one instrumentation.

A key list scales through globs rather than through compression: selecting a module is one selector
no matter how large the registry is, and unlike a fixed list it keeps covering functions added to
that module later. Where a subset genuinely is a hundred unrelated functions, it's a hundred lines
of configuration you can read, diff, and review — which is worth more than the bytes it costs, given
this is a config file rather than a wire format. (Earlier versions did compress: see
[Selecting functions by key](#selecting-functions-by-key) for why that was the wrong trade.)

### Running several at once

There's exactly one mechanism, so "several instrumentations in parallel" is just the general case of
what's above rather than a separate feature. Create as many as you need — each with its own enabled
subset and its own `Tracer` (potentially a different backend):

```rust
let checkout = Instrumentation::builder()
    .name("checkout-debug")
    .tracer(checkout_provider.tracer("checkout-debug"))
    .build()?;
let db_audit = Instrumentation::builder()
    .name("db-audit")
    .tracer(audit_provider.tracer("db-audit"))
    .build()?;

checkout.set_enabled(&["my_crate::checkout", "my_crate::orders::*"])?;
db_audit.set_enabled(&["my_crate::db::*"])?;
```

Each builds a fully independent trace from the same physical call chain: a function enabled for
both produces two spans, one per instrumentation, each parented within its own hierarchy. The
disabled fast path is unchanged — the extra machinery is paid only on calls where at least one
instrumentation is actually active.

`MAX_INSTRUMENTATIONS = 64` bounds how many may be **live at once**, not how many may be created
over the process lifetime: each handle owns one bit per `#[traceable]` site, and `Drop` releases
its slot to a free list for reuse. That matters for hot-reload, where an instrumentation is torn
down and rebuilt whenever its identity (tracer, endpoint) changes. `build()` returns
`Err(BuildError::SlotsExhausted)` only when 64 are concurrently live.

Slot reuse leaves one narrow, accepted race: `Drop` clears its bit across many sites
non-atomically, and a traced call reads a site's mask before reading the slot table. A thread
descheduled between those two reads, across an _entire_ drop and rebuild, could find the new
occupant's tracer behind a bit the old occupant set and emit one span into the wrong
instrumentation. Closing it properly would need epoch-based reclamation plus hot-path validation;
the cost of losing the race is a single mis-attributed span during a reload, which isn't worth that
machinery. See [`docs/multi-instrumentation.md`](docs/multi-instrumentation.md) for the design.

## In-process and distributed instrumentations

An instrumentation is one of two kinds. They are configured identically — same selectors, same
handle — and differ only in where a span finds its parent and where it leaves itself.

### In-process (the default)

Parents live in an `opentelemetry::Context` extension, one entry per live instrumentation, and
`Context`'s single "current span" slot is never read or written. That is what lets many run at
once over the same call flow, and it is also why they cannot propagate:

- **An incoming `traceparent` is not joined.** A propagator deposits the remote parent in
  `Context`'s span slot, which an in-process instrumentation never consults, so the first traced
  function on a call path always roots a fresh trace.
- **A span created outside opentelemetry-traceable is never a parent** either — not a web
  framework's server span, not a hand-rolled `tracer.start()`.
- **Outbound requests carry no `traceparent`**, since propagators inject whatever is in that same
  span slot.

An in-process instrumentation's spans nest only under other `#[traceable]` spans of that _same_
instrumentation. In-process nesting, including across `.await`, works correctly.

### Distributed (at most one)

```rust
let edge = Instrumentation::builder()
    .name("edge")
    .tracer(provider.tracer("edge"))
    .distributed()          // <- the only difference
    .build()?;
```

A distributed instrumentation uses `Context`'s current-span slot instead of the envelope. Its span
parents to whatever span is current — one a propagator extracted from an incoming `traceparent`,
one another library created, or its own previous span — and becomes the current span for the
wrapped call, so a propagator injecting from `Context::current()` inside that call carries it
outbound.

Propagation therefore works in both directions with **no propagator code in this crate**: your
application extracts and injects exactly as it already does, and `#[traceable]` spans join the
trace like any other spans.

Two consequences worth knowing before you turn one on:

- **It is visible to everything else.** Anything reading `Context::current().span()` —
  `tracing-opentelemetry`, an instrumented HTTP client, your own code — now sees these spans and
  nests under them. This is the point, but it is the one way this crate stops being invisible.
- **One logical trace, possibly two backends.** If the distributed instrumentation's tracer comes
  from a different provider than the rest of your app's, the trace's spans are split across two
  exporters. Legal in OpenTelemetry, surprising in a UI.

**Only one may be live at a time.** There is exactly one current-span slot per `Context`, so a
second distributed instrumentation would overwrite the first's span and silently merge two traces.
`build()` refuses with `Err(BuildError::DistributedAlreadyLive)`; dropping the live one releases
the role along with its slot. In-process instrumentations are unaffected — a distributed one takes
an ordinary slot, so the budget is 63 in-process plus 1 distributed.

The two kinds are fully isolated from each other: a distributed span never parents an in-process
one, or the other way round.

## Selecting functions by key

A selector is either an exact registry key or a glob. **`*` matches any run of characters,
including `::`** — that's the only wildcard, and there's deliberately no `**` counterpart, because
how deeply a function is nested is an implementation detail of the code being traced, not something
the person selecting it should have to track.

```text
my_app::domain::db::*   matches  my_app::domain::db::query
                        matches  my_app::domain::db::users::insert   (nested — `*` spans `::`)
                    does NOT match  my_app::domain::db               (nothing after the separator)
*::db::*                matches  any db function, in any crate or module
*                       matches  everything — the config-file equivalent of `enable_all()`
```

`opentelemetry_traceable::selector::resolve` is the entry point, and doubles as a dry run: it
resolves selectors against the registry without applying anything, so a glob can be previewed
before it's committed to a config file.

```rust
let selection = opentelemetry_traceable::selector::resolve(&["my_app::db::*"])?;
selection.keys;             // -> Vec<&'static str>, sorted and deduplicated
selection.unmatched_globs;  // -> globs that matched nothing
```

### Typos are errors, empty globs are warnings

An **exact** selector that matches nothing is almost certainly a typo, so resolution fails with
`UnknownKeys` (which names every offender, not just the first) and **nothing is applied** — the
instrumentation keeps whatever set it already had. Tracing an unnoticed subset of what was asked for
is worse than refusing outright, which is why the whole list is resolved before a single bit is
touched.

A **glob** that matches nothing only shows up in `Selection::unmatched_globs`, because it can
legitimately match nothing in a build where those functions were compiled out. If both happen at
once, the error wins and nothing is applied.

Two call sites can share a key — two methods with the same name in the same module, absent a `name`
override. One selector matches both, so they toggle together, and they're listed once.

"Enable everything" is `instr.enable_all()` from code, or `["*"]` from a config file. "Disable
everything" is an empty selector list.

### Why not ids

Earlier versions configured through a compact encoded string of numeric ids. That was dropped
because ids were silently invalidated by the most likely edit there is: adding or removing a
`#[traceable]` function renumbered every id after it, and a stale string still decoded to valid ids
now pointing at the _wrong_ functions, with nothing reported. A wrong key matches nothing and gets
named in an error. See [`docs/multi-instrumentation.md`](docs/multi-instrumentation.md) —
"History: the ids this replaced" — for the full record, including the two id schemes that came
before and what they cost.

## Letting an LLM (or a script) configure an arbitrary subset

This is the intended workflow when the thing picking which functions to trace has source access but
isn't running Rust, or is choosing from thousands of candidates:

1. **Dump the keys.** From within your instrumented application (an admin endpoint, a debug CLI
   flag, a one-off example — `opentelemetry-traceable` has no way to know how _your_ app wants to
   expose this, so it just provides the data):
   ```rust
   for key in opentelemetry_traceable::registry::keys() {
       println!("{key}");
   }
   ```
   That's every `#[traceable]` function currently linked into the binary, sorted:
   ```text
   kafka.fetch
   my_crate::process
   ```
   This is the _node_ list. Call-graph edges aren't known to the running binary — if the consumer
   needs them (to pick an entry point plus its transitive callees, say) it derives them from your
   source.
2. **Hand that list to the consumer** (an LLM with access to your source, a script, an operator)
   along with the task: "pick whichever of these functions should be traced."
3. **Apply the picks** to the instrumentation that should trace that subset — the keys go in
   directly, so there's no encoding step and nothing for the consumer to run against your binary
   first: `instr.set_enabled(&chosen_keys)?`.

Because keys are the identity, the consumer's output _is_ the configuration: it can write the keys
straight into a config file, and a glob lets it express "the whole `db` module" as one line rather
than enumerating it. A flow-shaped request — an entry point plus its transitive callees — still needs
the explicit list, since a flow isn't a module.

Worth exposing a dry run too, so a selection can be checked before it's committed:
`opentelemetry_traceable::selector::resolve(&picks)` reports exactly which keys matched and which
globs matched nothing, without touching any instrumentation. (`opentelemetry-traceable-demo` wires
both up as `keys` and `keys --select`.)

## For coding agents: `agents/AGENTS.md` and `agents/SKILL.md`

Two copy-paste templates for any application built on `opentelemetry-traceable`, so a coding agent asked
something like "trace the database" or "turn off tracing" can reconfigure it without reading
this whole README.

- `agents/AGENTS.md` → copy to the application repo's root as `AGENTS.md`.
- `agents/SKILL.md` → copy to the application repo as a manually-invoked Claude Code skill, e.g.
  `.claude/skills/configure-tracing/SKILL.md`.

Both drive the application's own key-listing entry point and read its source to derive the call
graph, so the selected set holds together as a hierarchy. They expect the app to expose that entry
point and ask the user if it doesn't.
See `opentelemetry-traceable-demo/AGENTS.md` and `opentelemetry-traceable-demo/.claude/skills/configure-tracing/SKILL.md` for a
working copy.

## Crate layout

- `opentelemetry-traceable` — runtime: `#[traceable]` re-export, an `opentelemetry` re-export
  (so consumers never declare it themselves — see Quick start), `registry` (the
  `linkme`-collected `TraceSite`/`REGISTRY`, one enabled bitmask per site),
  `instrumentation` (`Instrumentation` + its builder — creating, configuring, and dropping
  instrumentations, plus the `start_spans` hot path), `selector` (key/glob matching and
  `resolve`).
- `opentelemetry-traceable-macros` — the `#[traceable]` proc-macro implementation.

## Benchmarks

`opentelemetry-traceable/benches/traceable_overhead.rs` (Criterion) runs the same CPU-bound workload (1000
iterations) through a plain function, through `#[traceable]` with nothing tracing it, through
`#[traceable]` with one and then two live instrumentations (real in-process span exporters, not
no-op tracers), and through `#[tracing::instrument]` for comparison. Run with
`cargo bench -p opentelemetry-traceable`. Representative local numbers:

| variant                      | time     |
| ---------------------------- | -------- |
| `no_macro`                   | 862 ns   |
| `traceable_disa` (mask == 0) | 862 ns   |
| `traceable_one_instr_enab`   | 1.245 µs |
| `traceable_one_instr_disa`   | 871 ns   |
| `traceable_two_instr_enab`   | 1.488 µs |
| `traceable_two_instr_disa`   | 871 ns   |
| `tracing_instrument_disa`    | 931 ns   |
| `tracing_instrument_enab`    | 1.919 µs |

With no instrumentation tracing a function, `#[traceable]` costs the same as no macro at all,
within noise — the mask is `0` and the macro dispatch is a single atomic load followed by the raw
body. Enabled adds the real cost of span construction and export, once per active instrumentation.

Removing the special-cased default instrumentation cost the single-instrumentation case ~35 ns and
made every multi-instrumentation case faster — see
[`docs/multi-instrumentation.md`](docs/multi-instrumentation.md) — "Measured result" — for the
before/after numbers.

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
