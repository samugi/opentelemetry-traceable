---
name: configure-tracing
description: Turn opentelemetry-traceable tracing on or off for a subset of this app's functions, described in plain English (e.g. "trace the database", "trace the payment gateway", "turn off tracing"). Manually invoked -- run explicitly when asked to change what's traced.
---

This application uses `opentelemetry-traceable` for dynamic, per-function tracing: any `#[traceable]` function can
be turned on or off at runtime by editing a local config file. `args` is the request in plain
English (e.g. "trace the database"). Follow this procedure step by step, in order.

Each instrumentation in the config carries **two** lists of function keys: `enabled` (which
functions it traces at all) and `child_only` (which of those span only when that instrumentation
already has a recording span on the call path, never as a root — this keeps a shared helper from
emitting stray root spans on flows you weren't asked to trace). Every entry is a function's
**registry key**, written out in full, or a `*` glob over keys — exactly the matching functions
toggle, no false positives.

There is nothing to encode and no table of ids: the key *is* the function's identity, the way
DTrace names probes, so a correct list stays correct across rebuilds and across edits elsewhere
in the source. Still take keys from the current binary (step 1) — the registry is linked into
*it*, and an exact key that doesn't exist there is a hard error that leaves the whole update
unapplied.

`*` matches any run of characters, **including `::`** — the only wildcard, with no `**`. So
`my_app::domain::db::*` matches `my_app::domain::db::query` and the nested
`my_app::domain::db::pool::acquire`, but *not* `my_app::domain::db` itself (something must follow
the separator); `*::db::*` matches db functions in any module; `*` alone matches everything.

Two properties of `opentelemetry-traceable` constrain the sets you compute:

- Both lists belong to **one instrumentation**. There is no global or default one, and "already
  has a recording span" always means *a span from that same instrumentation*. If the config
  defines more than one and the user didn't say which, stop and ask.
- Only an enabled, **non**-child-only function can start a trace. `opentelemetry-traceable` does not join an
  incoming `traceparent`, and a span created outside `opentelemetry-traceable` (a framework's server span, a
  hand-rolled `tracer.start()`) is never a parent — so never count on an outer span to root the
  trace. If everything in your selection ends up child-only, nothing spans at all.

**Turning everything off needs none of the analysis below:** set both fields to the empty list
`[]` (step 5). To trace everything, set `enabled` to `['*']` and `child_only` to `[]`.

1. **List the keys** — every `#[traceable]` function in the binary, which is both the
   authoritative set of what can be traced and the exact spelling of each name you'll write into
   the config. Only the app's own binary can produce it (the registry is linked into *it*), so
   use whatever command this app exposes over `opentelemetry_traceable::registry::keys()`; for a Rust binary,
   typically:
   ```
   cargo run --quiet -- keys
   ```
   which prints one key per line. If there's no such command, **ask the user** how this app
   exposes its keys. Don't substitute a list grepped out of the source — an inferred key can be
   subtly wrong (a `name` override, a re-exported module), and an exact key that doesn't match
   rejects the whole update. If a `--select` form exists, use it to dry-run a selection before
   writing it:
   ```
   cargo run --quiet -- keys --select 'my_app::domain::db::*'
   ```
   It resolves the selectors against the same registry and prints what they match, changing no
   config and no tracing state.

2. **Work out the call graph around the request.** The key list is nodes only; edges come from
   reading the source, and they decide who can root a trace and who must be child-only. You only
   need the part of the graph the request touches.

   Map names to definitions first: a registry key is `module_path!() + "::" + fn_name`, or the
   string in `#[traceable(name = "...")]`. A key is *not* qualified by a surrounding `impl` type,
   so two same-named methods in one module share one key and toggle together.

   Then read each function in play and note which **other registered functions** it calls
   (`callees`; invert for `callers`):
   - **Direct calls** — the common case; match the callee against a key.
   - **Trait objects, `dyn` dispatch, function pointers, callbacks** — read enough source to find
     the implementations that can actually run on this path. If still ambiguous, include every
     plausible target key: an extra enabled function costs one extra span, a missed edge can
     silently break the hierarchy.
   - **Macro-generated calls** — resolve by reading the macro, or treat as ambiguous per above.
   - **Calls across a task or thread boundary** (`tokio::spawn`, `std::thread::spawn`, work sent
     over a channel) — **not** a parent/child edge. `opentelemetry-traceable` propagates through the ambient
     `opentelemetry::Context`, which a spawned task doesn't inherit, so a function first reached
     that way has no parent. Leave it out of `callers` and step 4 keeps it root-capable; mark it
     child-only and it goes silent instead.
   - **Recursion and cycles** — record the edges; step 4's cycle guard handles them.

3. **Build the enabled set `E`** (in keys):
   - **Module or concern** ("trace the database"): every function whose key is in that module
     (e.g. contains `::db::`). A glob says this in one line — `my_app::domain::db::*`, or
     `*::db::*` for a concern spread across modules — and keeps saying it as functions come and
     go. Prefer it here.
   - **Flow** ("trace checkout end-to-end"): start at the entry function and walk `callees`
     transitively — add each callee, and its callees, until nothing new appears.
   - **A concern you want rooted at real flows** (usually what "trace the database" actually
     wants): also walk `callers` upward from the concern functions, transitively. This pulls in
     the entry points so traces start where the work starts instead of dangling. Including it
     never hurts.

   A glob only collapses a request whose shape *is* a module. A flow is not a module — its
   members are scattered across handlers, services and helpers, and a module glob over any of
   them drags in siblings that aren't on the path — so a flow still needs the explicit key list
   you just derived. Same for `child_only` (step 4): it's a property of the call graph, not of a
   name, so glob it only when the whole matching set genuinely belongs there.

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

5. **Update the config, then verify.** Find the local config — commonly `config.yaml` /
   `config.json` at the repo root. The two fields belong to **one instrumentation**: expect a
   list of them (e.g. `instrumentations:`), each with its own `enabled` and `child_only`. Target
   the entry the user named; if there's more than one and they didn't say which, **ask**. Don't
   touch other entries, their identity fields (`name`/`service_name`/`otlp_endpoint`), or
   anything outside the list. If it isn't obviously the right file and fields, ask rather than
   guess. Both fields are **lists** of keys and globs (for a disable request, both `[]`); write
   the keys straight out of `E` and `CO`, order irrelevant, duplicates harmless, empty list
   selecting nothing. Setting a field *replaces* what's there; to add to what's on, include the
   previously-enabled keys in `E` too.

   **Then run the app and read its startup log.** Each instrumentation reports what it resolved
   to, as `[instr:<name>] enabled for N function(s): ...` (and the same for child-only). Confirm
   `N` and the listed keys are the set you intended, and that **no `matched no #[traceable]
   function` warning appeared** — that warning means a glob selected nothing, so what you meant
   it to cover isn't traced. A misspelled *exact* key is louder: it's an error, the whole
   selection is rejected, and the instrumentation keeps the set it already had. Don't report
   success until the log shows the set you computed.

6. **Report tersely.** Just confirm tracing was turned on or off for the requested domain, or
   that it failed and why. Nothing else — no function list, no mechanism explanation, no
   unsolicited follow-up.
