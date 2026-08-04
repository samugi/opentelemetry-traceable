# Configuring stylus tracing in this repo

> **Written for:** `stylus` @ `main` (this repo has no tagged releases yet, so `main` is the
> only version — it can gain new flags at any time). This file assumes `stylus-cli` supports the
> `encode` subcommand (`--ids`) and the `graph` subcommand (`--catalog`, `--src`). If
> `stylus-cli --help` doesn't list `graph`, or `stylus-cli encode` doesn't take `--ids`, your
> local copy of this file or your installed `stylus-cli` is stale — re-fetch this file from
> https://github.com/samugi/stylus (`agents/AGENTS.md`) and reinstall `stylus-cli` (step 1)
> before continuing.

This application uses [`stylus`](https://github.com/samugi/stylus) for dynamic, per-function
tracing: any `#[traceable]` function can be turned on/off at runtime by editing a local config
file. When asked to change what's traced (e.g. "trace the database", "turn off tracing"),
follow this exact procedure step by step. Don't write a new script, don't improvise a different
mechanism, and don't skip a step — everything you need is `stylus-cli`, a committed call-graph
catalog, and editing one config file.

The config has **two** encoded id lists:

- `enabled` — which functions trace at all.
- `child_only` — which of those are *child-only*: they only span when they already have an
  active parent span, never as a root. This is what keeps a shared helper (a db/cache call used
  by many flows) from orphaning as a stray root span on the flows you didn't ask to trace.

Each is a compact, lossless base64 encoding of a set of trace-site ids — no false positives,
exactly the listed functions toggle. An `id` is a function's index in the running binary's
registry, so ids (and any encoded value) are **specific to that build**: always use a catalog
from the current binary. You compute both sets mechanically from the catalog graph. Nothing here
requires understanding the code — it's graph traversal plus one membership rule.

## 1. Make sure `stylus-cli` is current

Run `stylus-cli --help` and confirm it lists both `encode` and `graph`. Then
`stylus-cli encode --help` and confirm it takes `--ids`.

- **All present**: good — continue.
- **`graph` missing, a flag missing, or the command isn't found**: (re)install it:
  ```
  cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
  ```

Checking first instead of always reinstalling is deliberate: reinstalling recompiles from
source every time, which is slow and usually unnecessary. The check protects you from a stale
binary (no version tags, so "already installed" doesn't mean "has what you need").

## Shortcut: enabling or disabling *everything*

- **Disable everything**: set *both* `enabled` and `child_only` to `""` (step 6). No catalog
  needed.
- **Enable everything**: there's no all-in-one shortcut anymore. Get the catalog (step 2), then
  set `enabled` to `stylus-cli encode --ids <every id in the catalog>` and `child_only` to `""`.
  Everything traces, and since every caller is enabled too, nothing orphans.

Otherwise, continue.

## 2. Get the call-graph catalog

You need a JSON catalog of every `#[traceable]` function **with its call-graph edges**
(`callers`/`callees` per function). This is what you traverse — no source-reading required for
the common case.

1. **Find the committed node dump.** A `{name, id}` list, commonly `stylus-catalog.json` or
   `catalog.json` at the repo root or `docs/`. If the name isn't obvious,
   `grep -rl '"functions"' . | xargs grep -l '"id"'` finds catalog-shaped JSON. **If there's
   none, stop and ask the user** to generate it (it's produced by the app's own binary —
   normally `cargo run --quiet -- catalog > stylus-catalog.json` — not something you can
   produce; see the note at the bottom for why). Regenerate it if it looks stale — ids must
   match the current binary.
2. **Make sure it has edges.** Check whether the functions have `callers`/`callees` fields.
   - **They do**: use the file as-is.
   - **They don't** (just `{name, id}`): add them yourself — this part you *can* do, it only
     needs the source:
     ```
     stylus-cli graph --catalog <the-node-dump>.json --src ./src > /tmp/stylus-graph.json
     ```
     and use `/tmp/stylus-graph.json` from here on. (`--src` is this app's source root, usually
     `./src`.)

The catalog also has an `unresolved_calls` list: call sites the static graph couldn't resolve
(dynamic dispatch, macros). You only consult it in step 4, and only for calls near your
selection.

## 3. Build the enabled set `E`

Work in terms of function **ids** from the catalog.

- **Module/concern request** (e.g. "trace the database"): `E` = every function whose name
  contains the module (e.g. `::db::`). Done.
- **Flow request** (e.g. "trace checkout end-to-end"): start with the entry function, then walk
  **`callees`** transitively — add each callee id, and its callees, until nothing new appears.
  `E` is everything reached.
- **Subject request where the target is shared/low-level** (e.g. "trace the database" but you
  want *complete* traces up to where each call originates): also walk **`callers`** upward from
  the concern functions — add each caller id, and its callers, until nothing new appears. This
  pulls in the entry points so the traces are rooted at real flows instead of dangling. (For a
  plain "trace the database" you can include this; it never hurts and prevents dangling db
  spans.)

## 4. Fill gaps near your selection (only if needed)

Scan `unresolved_calls` for entries whose `in_fn` is in `E` (or whose `candidates` are). For
each such entry — and *only* those, not the whole list — open the `site` (`file:line`), read
the few surrounding lines, and determine the real target function. If it resolves to a catalog
function that belongs in `E` by the step-3 logic (a callee on a flow you're tracing, or a
caller you're walking up to), add it. Ignore unresolved calls that don't touch your selection.

If `unresolved_calls` is empty, skip this step.

## 5. Compute the child-only set `CO`

Purely from `E` and the graph:

> **`CO` = every function in `E` that has at least one of its `callers` also in `E`.**
> Equivalently: a function is a *root* if none of its callers are in `E`; every other enabled
> function is child-only.

This is the whole rule. It gives exactly the right behavior both ways:

- Tracing a flow: the entry roots; every downstream function is child-only, so a shared helper
  stays silent (no orphan) on the *other* flows that also call it but aren't in `E`.
- Tracing a subject with ancestors walked in: the entry points root; the subject functions nest
  under them; and a subject function with no caller in `E` is a root, so it still produces a
  trace rather than vanishing.

**Cycle guard:** if two functions call each other and both are in `E`, the rule can mark both
child-only, leaving that group with no root (so nothing spans). After computing `CO`, make sure
every connected group within `E` has at least one root; if a pure cycle doesn't, remove the
requested entry point (or, if none, any one node of the cycle) from `CO`. Rare, but check.

## 6. Update the config, then verify

Generate the two encoded values (order of ids doesn't matter):

```
stylus-cli encode --ids <every id in E>          # -> enabled
stylus-cli encode --ids <every id in CO>         # -> child_only
```

Find the local YAML/JSON config — commonly `config.yaml`/`config.json` at the repo root, with
`tracing.enabled` and `tracing.child_only`. **If it isn't obviously the right file/fields, ask
the user — don't guess.** Set both fields (for a disable request, set both to `""`). This
replaces whatever was there; if asked to *add* to what's currently on, include the
previously-enabled ids in `E` too.

**Then re-read both fields and compare them character-for-character against what `encode`
printed.** Don't report success until they match exactly. A value off by even one character is
invalid and silently leaves tracing unchanged (a real, previously-seen failure mode: one
dropped character makes the encoded value fail to decode, and the app keeps its old state with
no visible effect).

## 7. Report

Just confirm: tracing was turned on/off for the requested domain, or that it failed and why.
Nothing else — no list of every function, no mechanism explanation, no unsolicited follow-up.

---

<details>
<summary>Why can't <code>stylus-cli</code> produce the node dump itself?</summary>

The `{name, id}` registry only exists inside the memory of a specific compiled binary that has
`#[traceable]` functions linked into it (via a linker-section trick). `stylus-cli` is a
separate, generic binary with none of that app's code in it, so it can't dump the node list —
only the app's own binary can. It *can* add the call-graph edges (`stylus-cli graph`), because
those come from parsing the source, which it has access to.

</details>
