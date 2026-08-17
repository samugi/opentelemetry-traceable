# Configuring opentelemetry-traceable tracing in this repo

This application uses [`opentelemetry-traceable`](https://github.com/samugi/opentelemetry-traceable) for dynamic, per-function
tracing: any `#[traceable]` function can be turned on or off at runtime by editing a local
config file. When asked to change what's traced (e.g. "trace the database", "trace checkout
end-to-end", "turn off tracing"), follow this procedure step by step.

Each instrumentation in the config carries **two** lists of function keys:

- `enabled` — which `#[traceable]` functions it traces at all.
- `child_only` — which of those are *child-only*: they span only when that instrumentation
  already has a recording span on the current call path, never as a root. This is what keeps a
  shared helper (a db/cache call reached from many flows) from emitting stray root spans on the
  flows you weren't asked to trace.

Every entry is a function's **registry key**, written out in full, or a `*` glob over keys —
exactly the matching functions toggle, no false positives. There is nothing to encode and no
table of ids to look up: the key *is* the function's identity, the way DTrace names probes. So
a list that is correct stays correct across rebuilds and across edits elsewhere in the source,
and stays readable in review. Still take keys from the current binary (step 1): the registry is
linked into *it*, and an exact key that doesn't exist there is a hard error that leaves the
whole update unapplied.

`*` matches any run of characters, **including `::`** — it is the only wildcard, and there is
no `**`. So `my_app::domain::db::*` matches both `my_app::domain::db::query` and the nested
`my_app::domain::db::pool::acquire`, but *not* `my_app::domain::db` itself (something must
follow the separator); `*::db::*` matches db functions in any module; `*` alone matches
everything.

Two properties of `opentelemetry-traceable` constrain the sets you compute:

- Both lists belong to **one instrumentation**. There is no global or default one — nothing
  traces unless an instrumentation carries these values — and "already has a recording span"
  always means *a span from that same instrumentation*. If the config defines more than one and
  the user didn't say which, stop and ask.
- Only an enabled, **non**-child-only function can start a trace. `opentelemetry-traceable` does not join an
  incoming `traceparent`, and a span created outside `opentelemetry-traceable` (a web framework's server span, a
  hand-rolled `tracer.start()`) is never a parent. Never count on an outer span to root the
  trace: if everything in your selection ends up child-only, nothing spans at all.

**Turning everything off needs none of the analysis below**: set both fields to the empty list
`[]` (step 5). To trace everything, set `enabled` to `['*']` and `child_only` to `[]`.

## 1. List the keys

The key list is every `#[traceable]` function in the binary — the authoritative set of what can
be traced, and the exact spelling of each name you will write into the config.

Only the application's own binary can produce it: the registry is built at link time from the
`#[traceable]` functions compiled into *it*. So use whatever command this app exposes over
`opentelemetry_traceable::registry::keys()`. For a Rust binary that's typically:

```
cargo run --quiet -- keys
```

which prints one key per line. If you can't find such a command, **ask the user** how this app
exposes its keys. Don't substitute a list you grepped out of the source — a key you inferred
from a file path can be subtly wrong (a `name` override, a re-exported module), and an exact key
that doesn't match rejects the whole update.

If the app also exposes a `--select` form, use it to preview a selection before writing it:

```
cargo run --quiet -- keys --select 'my_app::domain::db::*'
```

That resolves the selectors against the same registry and prints what they match, without
changing any config or tracing state — a real dry run for a glob you're unsure about.

## 2. Work out the call graph around the request

The key list is nodes only. Edges — who calls whom — come from reading the source, and they're
what decide which functions can root a trace and which must be child-only. You only need the
part of the graph your request touches, not the whole app.

First map names to definitions: a registry key is `module_path!() + "::" + fn_name`, or the
string in `#[traceable(name = "...")]`. A key is *not* qualified by a surrounding `impl` type, so
two same-named methods in one module share one key and always toggle together.

Then, for each function in play, read its body and note which **other registered functions** it
calls. That gives you `callees`; invert them for `callers`. The cases that decide whether a
hierarchy holds together:

- **Direct calls** — the common case. Match the callee against a key.
- **Trait objects, `dyn` dispatch, function pointers, callbacks** — read enough of the source to
  find the implementations that can actually run on this path. If it stays ambiguous, include
  every plausible target key: an extra enabled function costs one extra span, while a missed
  edge can silently break the hierarchy.
- **Macro-generated calls** — resolve by reading the macro, or treat as ambiguous per above.
- **Calls across a task or thread boundary** (`tokio::spawn`, `std::thread::spawn`, work handed
  to a channel and run elsewhere) — **not** a parent/child edge. `opentelemetry-traceable` propagates through the
  ambient `opentelemetry::Context`, which a spawned task does not inherit, so a function first
  reached that way starts with no parent. Leave it out of `callers` and step 4 will keep it
  root-capable; mark it child-only and it goes silent instead.
- **Recursion and cycles** — record the edges; step 4's cycle guard handles the consequence.

## 3. Build the enabled set `E`

Work in keys.

- **Module or concern** ("trace the database"): every function whose key is in that module
  (e.g. contains `::db::`). A glob says this in one line — `my_app::domain::db::*`, or
  `*::db::*` if the concern is spread across modules — and keeps saying it as functions are
  added or removed. Prefer it here.
- **Flow** ("trace checkout end-to-end"): start at the entry function and walk `callees`
  transitively — add each callee, and its callees, until nothing new appears.
- **A concern you want rooted at real flows** (usually what "trace the database" actually
  wants): also walk `callers` upward from the concern functions, transitively. This pulls in the
  entry points so traces start where the work starts instead of dangling at the db call. It
  never hurts to include this.

A glob only collapses requests whose shape *is* a module. A flow is not a module — its members
are scattered across handlers, services and helpers, and a module glob over any of them would
drag in siblings that aren't on the path — so a flow request still needs the explicit key list
you just derived. The same applies to `child_only` (step 4), which is a property of the call
graph rather than of a name; glob it only when the whole matching set genuinely belongs there.

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

## 5. Update the config, then verify

Find the local config — commonly `config.yaml` / `config.json` at the repo root. The two fields
belong to one instrumentation: expect a list of them (e.g. `instrumentations:`), each entry with
its own `enabled` and `child_only`. Target the entry the user named; if there's more than one and
they didn't say which, **ask**. Don't touch the other entries, their identity fields
(`name`/`service_name`/`otlp_endpoint`), or anything outside the list. If it isn't obviously the
right file and fields, ask rather than guess.

Both fields are **lists** of keys and globs (for a disable request, both `[]`). Write the keys
straight out of `E` and `CO` — there is nothing to encode. Order doesn't matter, duplicates are
harmless, and an empty list selects nothing. Setting a field *replaces* what was there — to add
to what's already on, include the previously-enabled keys in `E` too.

**Then run the app and read its startup log.** Each instrumentation reports what it resolved to,
as `[instr:<name>] enabled for N function(s): ...` (and the same for child-only). Confirm `N` and
the listed keys are the set you intended, and that **no `matched no #[traceable] function`
warning appeared** — that warning means a glob selected nothing, so whatever you meant it to
cover isn't being traced. A misspelled *exact* key is louder: it's an error, and the whole
selection is rejected, leaving the instrumentation with the set it already had. Don't report
success until the log shows the set you computed.

## 6. Report

Just confirm tracing was turned on or off for the requested domain, or that it failed and why.
Nothing else — no list of every function, no explanation of the mechanism, no unsolicited
follow-up.
