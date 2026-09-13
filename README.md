# opentelemetry-traceable

Fast, per-function, multi-instrumentation tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Allows generating multiple, independent traces where each span can be enabled/disabled at runtime.

## Quick start

Annotate the functions you may want to trace:

```rust
#[traceable]
fn process() { /* ... */ }

#[traceable(name = "checkout")]
async fn checkout_order() { /* ... */ }
```

Now tracing can be enabled individually for the `process` and `checkout_order` functions, at runtime.

Tracing is controlled using an **`Instrumentation`**.

Each `Instrumentation` binds:

1. A `Tracer`, which controls how the Instrumentation's traces are exported
2. An **enabled set** of functions, which controls the shape of this Instrumentation's traces

`Instrumentations` are isolated: it's possible to produce multiple/different traces off of the same function set by enabling different subsets of functions in different Instrumentations.

```rust
let instr = Instrumentation::builder()
    .tracer(provider.tracer("checkout-debug"))
    .build()?;

// the `process` function uses the default key: `my-crate::my-module::process`
// while `checkout_order` was renamed to `checkout` so it can be enabled
// without using a pattern:
instr.set_enabled(&["*::process", "checkout"])?;
```

### Test it out

```
cargo run --example basic -p opentelemetry-traceable
```

Check out the [`examples`](https://github.com/samugi/opentelemetry-traceable/tree/main/opentelemetry-traceable/examples) directory.

## Performance

Every `#[traceable]` function is disabled for every instrumentation by default. When no instrumentation is tracing a function, the annotation costs a single atomic load. The overhead is dominated by span creation, so for a given function, it is proportional to the number of instrumentations that `enable` that function.

```
Benchmark                      Mean (ns)
------------------------------ ---------
no_macro                          854.05
one_disabled                      856.19
one_enabled                      1209.67
tracing_instrument_disabled       963.46
tracing_instrument_enabled       1883.93
```

Benchmarked with Criterion. `no_macro` is the uninstrumented baseline, `one_enabled`/`one_disabled` show #[traceable] overhead with a single instrumentation toggled on/off, compared against tracing's #[instrument] macro under similar conditions.

## In-process and distributed instrumentations

An instrumentation is **in-process** by default: `in-process` `Instrumentations` are isolated, their spans can only be child of another span within the same `in-process` `Instrumentation`. Many `in-process` `Instrumentations` can coexist.

A `distributed` `Instrumentation` can be obtained by adding `.distributed()` to the builder.

```rust
let edge = Instrumentation::builder()
    .name("edge")
    .tracer(provider.tracer("edge"))
    .distributed()
    .build()?;
```

Unlike `in-process`, the `distributed` `Instrumentation` is unique, global, it interacts with OpenTelemetry's `current` Context, so it can join a propagated Context (e.g. via `traceparent`). Because distributed instrumentations are global, only one can exist at a time.

## Selecting functions by key

A selector is either an exact registry key or a pattern. `*` matches any set of characters.

```text
my_app::domain::db::*   matches  my_app::domain::db::query
                        matches  my_app::domain::db::users::insert
                        does NOT match  my_app::domain::db
*::db::*                matches  any db function, in any crate or module
*                       matches  everything
```

## Development

Tooling is managed with [`mise`](https://mise.jdx.dev/) (config at `.config/mise/config.toml`):

```
mise run test
mise run lint
```
