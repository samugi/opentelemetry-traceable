---
name: configure-tracing
description: Turn opentelemetry-traceable tracing on or off for a subset of this app's functions, described in plain English (e.g. "trace the database", "trace the payment gateway", "turn off tracing"). Manually invoked -- run explicitly when asked to change what's traced.
---

This application uses `opentelemetry-traceable` for dynamic, per-function tracing: any `#[traceable]` function can
be turned on or off at runtime by editing a local config file. `args` is the request in plain
English (e.g. "trace the database"). Follow this procedure step by step, in order.

Each instrumentation in the config carries **two** encoded id lists: `enabled` (which functions
it traces at all) and `child_only` (which of those span only when that instrumentation already
has a recording span on the call path, never as a root — this keeps a shared helper from
emitting stray root spans on flows you weren't asked to trace). Each is a lossless encoding of a
set of function **ids** — exactly the listed functions toggle, no false positives.

An id is a function's index in the sorted set of registry keys. Ids are stable across rebuilds,
but adding or removing a `#[traceable]` function renumbers them, so always take ids from a
catalog produced by the current binary (step 1).

Two properties of `opentelemetry-traceable` constrain the sets you compute:

- Both lists belong to **one instrumentation**. There is no global or default one, and "already
  has a recording span" always means *a span from that same instrumentation*. If the config
  defines more than one and the user didn't say which, stop and ask.
- Only an enabled, **non**-child-only function can start a trace. `opentelemetry-traceable` does not join an
  incoming `traceparent`, and a span created outside `opentelemetry-traceable` (a framework's server span, a
  hand-rolled `tracer.start()`) is never a parent — so never count on an outer span to root the
  trace. If everything in your selection ends up child-only, nothing spans at all.

**Turning everything off needs none of the analysis below:** set both fields to `""` (step 6).
To trace everything, get the catalog (step 1) and encode every id in it as `enabled`, with
`child_only` set to `""`.

1. **Get the catalog** — the `{name, id}` list of every `#[traceable]` function in the binary,
   which is both the authoritative set of what can be traced and the only source of ids. Only
   the app's own binary can produce it (the registry is linked into *it*), so use whatever
   command this app exposes over `opentelemetry_traceable::catalog::catalog_json()`; for a Rust binary, typically:
   ```
   cargo run --quiet -- catalog > /tmp/opentelemetry-traceable-catalog.json
   ```
   If there's no such command, **ask the user** how this app exposes its catalog. Don't
   substitute a list grepped out of the source — guessed ids silently trace the wrong functions.

2. **Work out the call graph around the request.** The catalog is nodes only; edges come from
   reading the source, and they decide who can root a trace and who must be child-only. You only
   need the part of the graph the request touches.

   Map names to definitions first: a registry key is `module_path!() + "::" + fn_name`, or the
   string in `#[traceable(name = "...")]`. A key is *not* qualified by a surrounding `impl` type,
   so two same-named methods in one module share one key and one id and toggle together.

   Then read each function in play and note which **other catalog functions** it calls
   (`callees`; invert for `callers`):
   - **Direct calls** — the common case; match the callee against a catalog name.
   - **Trait objects, `dyn` dispatch, function pointers, callbacks** — read enough source to find
     the implementations that can actually run on this path. If still ambiguous, include every
     plausible catalog target: an extra enabled function costs one extra span, a missed edge can
     silently break the hierarchy.
   - **Macro-generated calls** — resolve by reading the macro, or treat as ambiguous per above.
   - **Calls across a task or thread boundary** (`tokio::spawn`, `std::thread::spawn`, work sent
     over a channel) — **not** a parent/child edge. `opentelemetry-traceable` propagates through the ambient
     `opentelemetry::Context`, which a spawned task doesn't inherit, so a function first reached
     that way has no parent. Leave it out of `callers` and step 4 keeps it root-capable; mark it
     child-only and it goes silent instead.
   - **Recursion and cycles** — record the edges; step 4's cycle guard handles them.

3. **Build the enabled set `E`** (in catalog ids):
   - **Module or concern** ("trace the database"): every function whose name is in that module
     (e.g. contains `::db::`).
   - **Flow** ("trace checkout end-to-end"): start at the entry function and walk `callees`
     transitively — add each callee, and its callees, until nothing new appears.
   - **A concern you want rooted at real flows** (usually what "trace the database" actually
     wants): also walk `callers` upward from the concern functions, transitively. This pulls in
     the entry points so traces start where the work starts instead of dangling. Including it
     never hurts.

4. **Compute the child-only set `CO`** — purely from `E` and the edges:

   > **`CO` = every function in `E` that has at least one of its `callers` also in `E`.**
   > (A function is a *root* if none of its callers are in `E`; every other enabled function is
   > child-only.)

   That's the whole rule. Tracing a flow: the entry roots, everything downstream is child-only,
   so a shared helper stays silent on the *other* flows that call it. Tracing a concern with
   ancestors walked in: the entry points root, concern functions nest under them, and a concern
   function with no caller in `E` roots on its own, so it still shows rather than vanishing.

   **Cycle guard:** if two functions in `E` call each other, the rule can mark both child-only,
   leaving that group rootless (nothing spans). After computing `CO`, ensure every connected
   group in `E` has ≥1 root; if a pure cycle doesn't, drop the requested entry point (or any one
   cycle node) from `CO`.

5. **Encode both sets** with the app's own encode command — the same binary the catalog came
   from, since ids index its registry. Order of ids doesn't matter.
   ```
   cargo run --quiet -- encode --ids <every id in E>     # -> enabled
   cargo run --quiet -- encode --ids <every id in CO>    # -> child_only
   ```
   If the app exposes no encode command, it can be added in two lines over
   `opentelemetry_traceable::codec::encode`; ask the user rather than improvising an encoder.

6. **Update the config, then verify.** Find the local config — commonly `config.yaml` /
   `config.json` at the repo root. The two fields belong to **one instrumentation**: expect a
   list of them (e.g. `instrumentations:`), each with its own `enabled` and `child_only`. Target
   the entry the user named; if there's more than one and they didn't say which, **ask**. Don't
   touch other entries, their identity fields (`name`/`service_name`/`otlp_endpoint`), or
   anything outside the list. If it isn't obviously the right file and fields, ask rather than
   guess. Set both (for a disable request, both `""`). This *replaces* what's there; to add to
   what's on, include the previously-enabled ids in `E` too.

   **Then re-read both fields and compare them character-for-character against what `encode`
   printed.** Don't report success until they match exactly. A value off by one character fails
   to decode, and the app keeps its old state with no visible sign anything went wrong.

7. **Report tersely.** Just confirm tracing was turned on or off for the requested domain, or
   that it failed and why. Nothing else — no function list, no mechanism explanation, no
   unsolicited follow-up.
