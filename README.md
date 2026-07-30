# stylus

Dynamic, per-function tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Annotate any `fn`/`async fn` with `#[traceable]`, then enable or disable tracing for it at
runtime — individually, in bulk, or as an arbitrary subset of tens of thousands of functions —
without recompiling.

## Quick start

```rust
use stylus::traceable;

#[traceable]
fn process() { /* ... */ }

#[traceable(name = "kafka.fetch", tracer = "my-service")]
async fn fetch() { /* ... */ }

#[traceable(fields("component" = "proxy", "request_id" = id.clone()))]
async fn handle(id: String) { /* ... */ }
```

Every `#[traceable]` function is disabled by default. When disabled, a call costs a single
atomic load — no span, no `opentelemetry::Context` work at all. When enabled, span
creation and context propagation work exactly like manual `opentelemetry` instrumentation:
parent/child nesting is automatic via `opentelemetry::Context`'s ambient current-context
mechanism (attach/detach for sync, `FutureExt::with_context` across `.await` for async).
Disabling a function only skips *its own* span — it doesn't touch the ambient context, so an
enabled child still attaches correctly to the nearest actual open ancestor span, whether or not
everything in between is enabled.

Registry key (the string you enable/disable by) defaults to `module_path!() + "::" + fn_name`,
or the `name` argument if given. Note this isn't qualified by a surrounding `impl` type — two
methods with the same name in the same module share a key unless `name` disambiguates them.

## Avoiding orphan spans for functions shared across call paths

A function's `enabled` flag is global — it fires for *every* caller, not just the one you had
in mind. That's fine for a function with one call site, but a function shared across multiple
call paths (a common `db`/`cache`/logging-style helper called from several different flows)
will also fire — as a disconnected root span — every time some *other*, non-traced path calls
it too, since disabling doesn't touch the ambient context and there's nothing above it to
attach to.

The fix is *child-only mode*: a function in this mode only creates a span when it's called from
within an already-active (recording) span — never a root, even when enabled.

```rust
stylus::config::set_child_only(["my_crate::shared_helper"]);
// or from a compact blob, exactly like the enabled set:
stylus::config::set_child_only_encoded(&blob)?;
```

Crucially this is **not** a source annotation — it's set at runtime, because whether a shared
helper *should* root a trace depends on what you're tracing. Tracing the flow that calls it?
Put it in child-only mode so it stays nested and never orphans on the *other* flows. Tracing
the helper's own subsystem (e.g. "trace the database")? Leave it root-capable so it still
produces a trace even when its immediate caller isn't traced. It only changes *when* a span is
created, not whether — still zero false negatives on the path you enabled, still the same
near-zero cost when off.

Deciding which functions to put in child-only mode for a given request is mechanical given a
call graph: a function should be child-only exactly when one of its own callers is also being
traced (so it always has a parent), and root-capable otherwise. That's what `stylus-cli graph`
and the agent workflow below automate.

## Configuring what's enabled

```rust
stylus::config::enable(["my_crate::process", "kafka.fetch"]);
stylus::config::disable(["my_crate::process"]);
stylus::config::set_enabled(["kafka.fetch"]); // replace the whole active set
stylus::config::enable_all();
stylus::config::disable_all();
stylus::config::is_enabled("kafka.fetch"); // -> bool
stylus::config::all_names(); // -> every registered key, for introspection

stylus::config::set_child_only(["my_crate::shared_helper"]); // replace the child-only set
stylus::config::child_only_names(); // -> those currently in child-only mode
```

This works fine for a handful of functions. It doesn't scale as a wire format: 100 names out of
a 100,000-function registry is 4-6 KB of configuration just to select 0.1% of it. That's what
the rest of this document is for.

## Multiple parallel instrumentations

Everything above drives the single, always-present **default instrumentation**. On top of it you
can create named instrumentations at runtime — each with its own enabled subset, its own isolated
span hierarchy over the same call flow, and its own `Tracer` (potentially a different backend):

```rust
let checkout = stylus::instrumentation::Instrumentation::builder()
    .name("checkout-debug")
    .tracer(provider.tracer("checkout-debug")) // any opentelemetry Tracer
    .build()?;
checkout.enable(["my_crate::checkout", "my_crate::db::insert"]);
// ... same enable/disable/set_child_only/*_encoded API as `stylus::config`, scoped to this handle.
// Dropping `checkout` stops new spans for it and frees the slot.
```

The default and each named instrumentation build fully independent traces from the same physical
call chain. The disabled and default-only fast paths are unchanged — the extra machinery is paid
only on calls where a named instrumentation is actually active. Up to 64 concurrent
instrumentations; named ones are in-process only (the default owns downstream `traceparent`
propagation). See [`docs/multi-instrumentation.md`](docs/multi-instrumentation.md) for the design
and the measured no-regression numbers.

## Compact subset encoding

`stylus::subset` encodes an arbitrary selection of functions as a **Bloom filter** instead of a
name list. It tests membership directly against each function's own registry key — there's no
positional index and no manifest to keep in sync with the registry, so a blob stays valid even
if the registry's size or link order changes between builds.

```rust
let blob = stylus::subset::encode(["my_crate::process", "kafka.fetch"], 0.01); // 0.01 = 1% target false-positive rate
stylus::config::set_enabled_encoded(&blob)?;   // replace, like set_enabled
stylus::config::enable_encoded(&blob)?;        // additive, like enable
stylus::config::disable_encoded(&blob)?;       // subtractive, like disable
```

For 100 of 100,000 functions at a 1% false-positive rate, that's ~120 bytes (~160 base64
chars) — about 30x smaller than the name list. The tradeoff: a Bloom filter guarantees **zero
false negatives** (every function you asked for really is enabled) at the cost of a small,
tunable chance of **false positives** (a handful of *other* functions also end up traced). If
you need exact behavior with no false positives at all, don't use this — build up the active
set with the exact-name functions above instead.

To enable (or disable) *everything* without enumerating every id, use
`stylus::subset::encode_all()` instead of `encode`/`encode_ids`: a Bloom filter with every bit
set matches any id unconditionally, so it's just the smallest possible valid blob (~13 base64
chars) that happens to be a superset of the whole registry — no new code path, no sentinel
value, it flows through `set_enabled_encoded`/`enable_encoded`/`disable_encoded` exactly like
any other blob.

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
   same 64-bit id `stylus::subset` uses internally:
   ```json
   {
     "hash": "fnv1a64",
     "index_formula": "let h1 = id as u32; let h2 = (id >> 32) as u32; idx_i = h1.wrapping_add(i * h2) % m, for i in 0..k",
     "functions": [
       { "name": "my_crate::process", "id": 11212198487925888491 },
       { "name": "kafka.fetch", "id": 4108071546255015497 }
     ]
   }
   ```
   This is the *node* list. Call-graph edges aren't known to the running binary; run
   `stylus-cli graph` (below) over your source to augment this with `callers`/`callees` per
   function (plus an `unresolved_calls` worklist) — needed to pick enabled ancestors and
   child-only functions that keep the trace hierarchy intact.
2. **Hand that JSON to the consumer** (an LLM with access to your source, a script, an
   operator) along with the task: "pick whichever of these functions should be traced."
3. **Turn the picks into a blob.** Either in Rust:
   ```rust
   let blob = stylus::subset::encode_ids(chosen_ids, 0.01);
   // or, from names instead of pre-looked-up ids:
   let blob = stylus::subset::encode(chosen_names, 0.01);
   ```
   or without writing any Rust at all, via the CLI (see below).
4. **Apply it**: `stylus::config::set_enabled_encoded(&blob)?`.

### The encoding, if you need to reproduce it without this crate

Simple enough to reimplement in a short script, given a catalog dump's `{name, id}` list:

1. **Hash**: 64-bit FNV-1a over the name's UTF-8 bytes (offset basis `0xcbf29ce484222325`,
   prime `0x100000001b3`) — this is the `id` in the catalog.
2. **Sizing**: for `n` items at target false-positive rate `p`:
   `m = ceil(-n * ln(p) / ln(2)^2)` bits, `k = round((m / n) * ln(2))` hash rounds.
3. **Bit positions**: `h1 = id as u32`, `h2 = (id >> 32) as u32`. For `i` in `0..k`:
   `idx_i = h1.wrapping_add(i * h2) % m` (Kirsch–Mitzenmacher double hashing) — set/check bit
   `idx_i`.
4. **Wire format**: `[u32 m, little-endian][u8 k][ceil(m/8) bytes, bit i at byte i/8, bit i%8]`,
   then base64 (URL-safe, unpadded).

## `stylus-cli`

A standalone binary for turning a name or id list into a blob without writing any Rust:

```
stylus-cli encode --names my_crate::process kafka.fetch [--fp-rate 0.01]
stylus-cli encode --ids 11212198487925888491 4108071546255015497 [--fp-rate 0.01]
stylus-cli encode --all   # match every function, no names/ids needed
```

Reads from stdin (one entry per line) if no values are given as arguments (and neither `--all`
is passed). Prints the base64 blob to stdout. It's standalone — it only hashes/encodes what
it's given; it has no way to introspect any particular application's registry. To get the list
of functions your application actually knows about (and their ids) for a human or LLM to pick
from, call `stylus::catalog::catalog_json()` from within that application, as described above.

It also builds the call graph, by statically parsing your source (no compile, no run):

```
stylus-cli graph --catalog catalog.json --src ./src
```

This takes the node dump (from `catalog_json()`) and prints it back with `callers`/`callees`
per function, resolving direct calls between `#[traceable]` functions by trailing-path match.
Calls it can't resolve statically — dynamic dispatch, function pointers, macro-generated
calls — are listed under `unresolved_calls` (a `file:line` worklist) rather than silently
dropped, so a source-aware agent can fill in just the ones relevant to its request. It's exact
for direct calls (see the `unresolved_calls` note for the blind spots).

## Convention: commit a catalog file, don't assume a command

`stylus-cli` can never dump an application's catalog itself — the `{name, id}` registry only
exists inside the memory of a specific compiled binary that actually has `#[traceable]`
functions linked into it (that's the whole reason `stylus::catalog::catalog_json()` is a library
call the app makes itself, not a `stylus-cli` subcommand). Rather than standardizing on some
fixed CLI invocation and hoping every app's binary happens to support it the same way, the
convention is simpler and more robust: **commit the catalog as a JSON file in the repo** (e.g.
`stylus-catalog.json` at the root — see `stylus-demo`'s copy), produced once via
`stylus::catalog::catalog_json()` from within the app however that app chooses to expose it, and
regenerated whenever its `#[traceable]` functions change.

A coding agent reconfiguring tracing should look for this committed file first. If it isn't
there, or isn't easy to find, it should **ask the user to produce one** (using their app's own
`stylus::catalog::catalog_json()`) and provide it — not guess a command and run it unprompted.

## For coding agents: `agents/AGENTS.md` and `agents/SKILL.md`

Two copy-paste templates for any application built on `stylus`, so a coding agent (asked
something like "trace the database" or "turn off tracing") can reconfigure it using *only*
`stylus-cli encode` — no bespoke scripts, no reading this whole README.

- `agents/AGENTS.md` → copy to the application repo's root as `AGENTS.md`.
- `agents/SKILL.md` → copy to the application repo as a manually-invoked Claude Code skill, e.g.
  `.claude/skills/configure-tracing/SKILL.md`.

Both expect a catalog JSON file to already be committed in the repo (per the convention above).
If it's missing or not obviously located, they ask the user to produce and provide one, rather
than trying to generate it themselves. See `stylus-demo/AGENTS.md` and
`stylus-demo/.claude/skills/configure-tracing/SKILL.md` for a working copy.

## Crate layout

- `stylus` — runtime: `#[traceable]` re-export, `registry` (the `linkme`-collected
  `TraceSite`/`REGISTRY`), `config` (enable/disable, exact and encoded), `subset` (Bloom filter
  encode/decode), `catalog` (the `{name, id}` dump).
- `stylus-macros` — the `#[traceable]` proc-macro implementation.
- `stylus-cli` — the standalone binary described above (built with `clap`): `encode` (names/ids
  → blob) and `graph` (source → call-graph edges, via `syn`).

## Benchmarks

`stylus/benches/traceable_overhead.rs` (Criterion) compares the same CPU-bound workload across
three shapes: a plain function with no macro, `#[traceable]` with tracing disabled, and
`#[traceable]` with tracing actually enabled (a real in-process span exporter, not a no-op
tracer). Run with `cargo bench -p stylus`. Representative local numbers:

| variant                        | time      |
| ------------------------------ | --------- |
| plain (no macro)                | ~865 ns  |
| `#[traceable]`, disabled         | ~871 ns  |
| `#[traceable]`, enabled           | ~1.26 µs |

Disabled costs the same as no macro at all, within noise; enabled adds the real cost of span
creation and export.

## Development

Tooling is managed with [`mise`](https://mise.jdx.dev/) (config at `.config/mise/config.toml`):

```
mise run test   # cargo nextest run --workspace
mise run lint   # cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Tests must be run via `cargo nextest run`, not plain `cargo test` — several tests mutate
process-global state (the OTel tracer provider, the `linkme` registry), and rely on nextest's
process-per-test isolation instead of a shared-process `Mutex`. Each crate with tests has a
`check_test_runner` test that fails with a clear message if it detects it's running under plain
`cargo test`.

CI (`.github/workflows/ci.yml`) runs `rust-fmt`, `rust-clippy`, `rust-doc`, and `rust-test`
(via `mise run test`) as separate jobs on every push/PR.
