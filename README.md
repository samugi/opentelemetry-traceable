# opentelemetry-traceable

Fast, per-function, multi-instrumentation tracing on top of [`opentelemetry-rust`](https://github.com/open-telemetry/opentelemetry-rust).
Allows generating multiple, independent traces where each span can be enabled/disabled at runtime.

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
own `Tracer`, and configures an _enabled subset_ of functions, thus controlling its own trace shape. `Instrumentations` are therefore isolated, and it's possible to produce multiple concurrent and different traces off of the same function set, which are treated independently during export.

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
