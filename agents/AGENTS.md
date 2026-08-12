# Configuring opentelemetry-traceable tracing in this repo

This application uses [`opentelemetry-traceable`](https://github.com/samugi/opentelemetry-traceable) for dynamic, per-function
tracing: any `#[traceable]` function can be turned on or off at runtime by editing a local
config file. When asked to change what's traced (e.g. "trace the database", "trace checkout
end-to-end", "turn off tracing"), follow this procedure step by step.

Each instrumentation in the config carries **two** encoded id lists:

- `enabled` — which `#[traceable]` functions it traces at all.
- `child_only` — which of those are *child-only*: they span only when that instrumentation
  already has a recording span on the current call path, never as a root. This is what keeps a
  shared helper (a db/cache call reached from many flows) from emitting stray root spans on the
  flows you weren't asked to trace.

Each is a lossless encoding of a set of function **ids** — exactly the listed functions toggle,
no false positives. An id is a function's index in the sorted set of registry keys. Ids are
stable across rebuilds, but adding or removing a `#[traceable]` function renumbers them, so
always take ids from a catalog produced by the current binary (step 1).

Two properties of `opentelemetry-traceable` constrain the sets you compute:

- Both lists belong to **one instrumentation**. There is no global or default one — nothing
  traces unless an instrumentation carries these values — and "already has a recording span"
  always means *a span from that same instrumentation*. If the config defines more than one and
  the user didn't say which, stop and ask.
- Only an enabled, **non**-child-only function can start a trace. `opentelemetry-traceable` does not join an
  incoming `traceparent`, and a span created outside `opentelemetry-traceable` (a web framework's server span, a
  hand-rolled `tracer.start()`) is never a parent. Never count on an outer span to root the
  trace: if everything in your selection ends up child-only, nothing spans at all.

**Turning everything off needs none of the analysis below**: set both fields to `""` (step 6).
To trace everything, get the catalog (step 1) and encode every id in it as `enabled`, with
`child_only` set to `""`.

## 1. Get the catalog

The catalog is the `{name, id}` list of every `#[traceable]` function in the binary — the
authoritative set of what can be traced, and the only source of ids.

Only the application's own binary can produce it: the registry is built at link time from the
`#[traceable]` functions compiled into *it*. So use whatever command this app exposes over
`opentelemetry_traceable::catalog::catalog_json()`. For a Rust binary that's typically:

```
cargo run --quiet -- catalog > /tmp/opentelemetry-traceable-catalog.json
```

If you can't find such a command, **ask the user** how this app exposes its catalog. Don't
substitute a list you grepped out of the source — a name without the binary's id is useless, and
guessed ids silently trace the wrong functions.

## 2. Work out the call graph around the request

The catalog is nodes only. Edges — who calls whom — come from reading the source, and they're
what decide which functions can root a trace and which must be child-only. You only need the
part of the graph your request touches, not the whole app.

First map names to definitions: a registry key is `module_path!() + "::" + fn_name`, or the
string in `#[traceable(name = "...")]`. A key is *not* qualified by a surrounding `impl` type, so
two same-named methods in one module share one key and one id and always toggle together.

Then, for each function in play, read its body and note which **other catalog functions** it
calls. That gives you `callees`; invert them for `callers`. The cases that decide whether a
hierarchy holds together:

- **Direct calls** — the common case. Match the callee against a catalog name.
- **Trait objects, `dyn` dispatch, function pointers, callbacks** — read enough of the source to
  find the implementations that can actually run on this path. If it stays ambiguous, include
  every plausible catalog target: an extra enabled function costs one extra span, while a missed
  edge can silently break the hierarchy.
- **Macro-generated calls** — resolve by reading the macro, or treat as ambiguous per above.
- **Calls across a task or thread boundary** (`tokio::spawn`, `std::thread::spawn`, work handed
  to a channel and run elsewhere) — **not** a parent/child edge. `opentelemetry-traceable` propagates through the
  ambient `opentelemetry::Context`, which a spawned task does not inherit, so a function first
  reached that way starts with no parent. Leave it out of `callers` and step 4 will keep it
  root-capable; mark it child-only and it goes silent instead.
- **Recursion and cycles** — record the edges; step 4's cycle guard handles the consequence.

## 3. Build the enabled set `E`

Work in catalog ids.

- **Module or concern** ("trace the database"): every function whose name is in that module
  (e.g. contains `::db::`).
- **Flow** ("trace checkout end-to-end"): start at the entry function and walk `callees`
  transitively — add each callee, and its callees, until nothing new appears.
- **A concern you want rooted at real flows** (usually what "trace the database" actually
  wants): also walk `callers` upward from the concern functions, transitively. This pulls in the
  entry points so traces start where the work starts instead of dangling at the db call. It
  never hurts to include this.

## 4. Compute the child-only set `CO`

Purely from `E` and the edges:

> **`CO` = every function in `E` that has at least one of its `callers` also in `E`.**
> Equivalently: a function is a *root* if none of its callers are in `E`; every other enabled
> function is child-only.

That's the whole rule, and it gives the right behavior both ways. Tracing a flow: the entry
roots, everything downstream is child-only, so a shared helper stays silent on the *other* flows
that call it but aren't in `E`. Tracing a concern with ancestors walked in: the entry points
root, the concern functions nest under them, and a concern function with no caller in `E` roots
on its own, so it still shows up rather than vanishing.

**Cycle guard:** if two functions in `E` call each other, the rule can mark both child-only,
leaving that group with no root, so nothing spans. After computing `CO`, check that every
connected group within `E` has at least one root; if a pure cycle doesn't, drop the requested
entry point (or, failing that, any one node of the cycle) from `CO`.

## 5. Encode both sets

Use the app's own encode command — the same binary the catalog came from, since ids index its
registry. Typically:

```
cargo run --quiet -- encode --ids <every id in E>      # -> enabled
cargo run --quiet -- encode --ids <every id in CO>     # -> child_only
```

Order of ids doesn't matter. If the app exposes no encode command, it can be added in two lines
over `opentelemetry_traceable::codec::encode`; ask the user rather than improvising an encoder.

## 6. Update the config, then verify

Find the local config — commonly `config.yaml` / `config.json` at the repo root. The two fields
belong to one instrumentation: expect a list of them (e.g. `instrumentations:`), each entry with
its own `enabled` and `child_only`. Target the entry the user named; if there's more than one and
they didn't say which, **ask**. Don't touch the other entries, their identity fields
(`name`/`service_name`/`otlp_endpoint`), or anything outside the list. If it isn't obviously the
right file and fields, ask rather than guess.

Set both fields (for a disable request, both `""`). This *replaces* what was there — to add to
what's already on, include the previously-enabled ids in `E` too.

**Then re-read both fields and compare them character-for-character against what `encode`
printed.** Don't report success until they match exactly. A value off by one character fails to
decode, and the app keeps its old state with no visible sign anything went wrong.

## 7. Report

Just confirm tracing was turned on or off for the requested domain, or that it failed and why.
Nothing else — no list of every function, no explanation of the mechanism, no unsolicited
follow-up.
