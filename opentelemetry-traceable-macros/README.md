# opentelemetry-traceable-macros

Proc-macro crate for [`opentelemetry-traceable`](https://crates.io/crates/opentelemetry-traceable).

This crate provides the `#[traceable]` attribute macro. Depend on `opentelemetry-traceable`.

```rust
use opentelemetry_traceable::traceable;

#[traceable]
fn process() { /* ... */ }
```

See the [`opentelemetry-traceable` documentation](https://crates.io/crates/opentelemetry-traceable)
for usage, configuration, and examples.
