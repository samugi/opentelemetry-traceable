# opentelemetry-traceable

Dynamic, per-function multi-instrumentation tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Annotate any `fn`/`async fn` with `#[traceable]`, then enable or disable tracing for them at
runtime, individually (by exact name), or in bulk (using patterns).

## Quick start

Annotate the functions you may want to trace:

```rust
use opentelemetry_traceable::traceable;

#[traceable]
fn process() { /* ... */ }

#[traceable(name = "data.fetch")]
async fn fetch() { /* ... */ }

#[traceable(fields("component" = "proxy", "request_id" = id.clone()))]
async fn handle(id: String) { /* ... */ }
```

Tracing is controlled using an **`Instrumentation`**. Each `Instrumentation` brings its
own `Tracer`, and configures an _enabled subset_ of functions, thus controlling its own trace shape. This means `Instrumentations` are isolated, so that it's possible to produce multiple concurrent and different traces off of the same function set, which are each treated individually during export, thanks to the dedicated `Tracers`.

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
    .tracer(provider.tracer("checkout-debug"))
    .build()?;

instr.set_enabled(&["my_crate::checkout::*"])?;
```

Every `#[traceable]` function is disabled for every instrumentation by default. When no
instrumentation is tracing a function, a call costs a single atomic load.

An instrumentation is **in-process** by default: its spans hierarchy is isolated, and a span can only be the child of another span within the same `in-process` `Instrumentation`.

A `distributed` `Instrumentation` can be obtained by adding `.distributed()` to the builder.
The `distributed` `Instrumentation` is unique, global, it interacts with OpenTelemetry's `current` Context, so it can join a propagated Context (e.g. via `traceparent`). See
[In-process and distributed instrumentations](#in-process-and-distributed-instrumentations).

## Configuring an instrumentation

An instrumentation is configured by **listing the functions it should trace**.
This can be done by registry keys, or `*` patterns.

```rust
// replace the enabled set with the newly provided one
instr.set_enabled(&["my_crate::checkout"])?;
// add the provided pattern to the previously enabled set
instr.enable(&["my_crate::db::*"])?;
```

### Running several at once

Several instrumentations can be enabled and run together.
Each builds a fully independent trace from the same call chain.

There is a limit of `64` max instrumentations: this limits how many instrumentations can be **live at once**.

## In-process and distributed instrumentations

### In-process (default)

Completely isolated and not resumable

- **An inbound (propagated) `Context` cannot be joined.**
- **A trace created outside opentelemetry-traceable is never joined**.
- **Outbound requests carry no `Context`**.

An in-process instrumentation's spans nest only under other `#[traceable]` spans of that _same_
instrumentation.

### Distributed (at most one)

```rust
let edge = Instrumentation::builder()
    .name("edge")
    .tracer(provider.tracer("edge"))
    .distributed()
    .build()?;
```

A distributed instrumentation uses the current `Context`'s span slot. Its span
becomes a child of whatever span is "current", so propagators are compatible with this
instrumentation as they interact with the `span` field of the current `Context`.

For propagation to work, your application must extract and inject from/to `Context::current`,
and `#[traceable]` spans join the trace like any other spans.

Because distributed instrumentations are global, only one can exist at a time.

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
