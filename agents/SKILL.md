---
name: configure-tracing
description: Turn stylus tracing on or off for a subset of this app's functions, described in plain English (e.g. "trace the database", "trace the payment gateway", "turn off tracing"). Manually invoked -- run explicitly when asked to change what's traced.
---

> **Written for:** `stylus` @ `main` (this repo has no tagged releases yet — `main` is the only
> version, and it can gain new flags at any time). This skill assumes `stylus-cli` has both an
> `encode` subcommand (`--names`, `--ids`, `--all`, `--fp-rate`) and a `graph` subcommand
> (`--catalog`, `--src`). If `stylus-cli --help` doesn't list `graph`, or an `encode` flag is
> missing, this file or your installed `stylus-cli` is stale — re-fetch it from
> https://github.com/samugi/stylus (`agents/SKILL.md`) and reinstall `stylus-cli` (step 1).

This application uses `stylus` for dynamic, per-function tracing: any `#[traceable]` function
can be turned on/off at runtime by editing a local config file. `args` is the request in plain
English (e.g. "trace the database"). Follow this procedure step by step, in order. Do not write
a new script or improvise a different mechanism — everything needed is `stylus-cli`, a committed
call-graph catalog, and editing one config file.

The config holds **two** blobs: `enabled_blob` (which functions trace at all) and
`child_only_blob` (which enabled functions only span when they already have an active parent,
never as a root — this keeps a shared helper from orphaning on flows you didn't ask to trace).
You compute both mechanically from the catalog graph: graph traversal plus one membership rule,
no code comprehension required.

1. **Make sure `stylus-cli` is current.** Run `stylus-cli --help` and confirm it lists both
   `encode` and `graph`; then `stylus-cli encode --help` and confirm `--names`, `--ids`,
   `--all`, `--fp-rate`.
   - All present: continue.
   - `graph` missing, a flag missing, or the command isn't found: (re)install it:
     ```
     cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
     ```
   Checking first instead of always reinstalling is deliberate: reinstalling recompiles every
   time (slow, usually unnecessary). The check protects you from a stale binary (no version
   tags, so "already installed" doesn't mean "has what you need").

   **Shortcut for "enable/disable everything":** if `args` asks to trace everything, set
   `enabled_blob` to `stylus-cli encode --all` and `child_only_blob` to `""` (step 6) — no
   catalog needed. To disable everything, set *both* blobs to `""` (step 6). Otherwise continue.

2. **Get the call-graph catalog.** You need a JSON catalog of every `#[traceable]` function
   *with its call-graph edges* (`callers`/`callees` per function) — that's what you traverse.
   1. Find the committed node dump: a `{name, id}` list, commonly `stylus-catalog.json` /
      `catalog.json` at the repo root or `docs/` (`grep -rl '"hash": "fnv1a64"' .` finds it).
      **If there's none, stop and ask the user** to generate it — it comes from the app's own
      binary (normally `cargo run --quiet -- catalog > stylus-catalog.json`), not something you
      can produce.
   2. Check whether its functions have `callers`/`callees`.
      - They do: use it as-is.
      - They don't (just `{name, id}`): add them yourself (this only needs the source):
        ```
        stylus-cli graph --catalog <node-dump>.json --src ./src > /tmp/stylus-graph.json
        ```
        and use `/tmp/stylus-graph.json` from here on.

   The catalog also has an `unresolved_calls` list (calls static analysis couldn't resolve);
   you only touch it in step 4.

3. **Build the enabled set `E`** (work in function ids):
   - **Module/concern** ("trace the database"): `E` = every function whose name contains the
     module (e.g. `::db::`). Done.
   - **Flow** ("trace checkout end-to-end"): start at the entry function and walk `callees`
     transitively — add each callee, and its callees, until nothing new appears.
   - **Subject you want fully rooted** (e.g. complete traces ending in db calls): also walk
     `callers` upward from the concern functions — add each caller, and its callers, until
     nothing new appears. This pulls in the entry points so traces are rooted at real flows
     rather than dangling. (Including this for a plain "trace the database" never hurts.)

4. **Fill gaps near your selection (only if needed).** Scan `unresolved_calls` for entries whose
   `in_fn` (or `candidates`) are in `E`. For *those only*, open the `site` (`file:line`), read
   the few surrounding lines, resolve the real target, and add it to `E` if step-3 logic says it
   belongs (a callee on a traced flow, or a caller you're walking up to). Ignore unresolved
   calls that don't touch your selection. If `unresolved_calls` is empty, skip this step.

5. **Compute the child-only set `CO`** — purely from `E` and the graph:

   > **`CO` = every function in `E` that has at least one of its `callers` also in `E`.**
   > (A function is a *root* if none of its callers are in `E`; every other enabled function is
   > child-only.)

   That's the whole rule. Tracing a flow: the entry roots, everything downstream is child-only,
   so a shared helper stays silent on the *other* flows that also call it. Tracing a subject
   with ancestors walked in: the entry points root, subject functions nest under them, and a
   subject function with no caller in `E` roots (so it still shows, never vanishes).

   **Cycle guard:** if two functions in `E` call each other, the rule can mark both child-only,
   leaving that group rootless (nothing spans). After computing `CO`, ensure every connected
   group in `E` has ≥1 root; if a pure cycle doesn't, drop the requested entry point (or any one
   cycle node) from `CO`.

6. **Update the config, then verify.** Generate both blobs:
   ```
   stylus-cli encode --ids <every id in E>     # -> enabled_blob
   stylus-cli encode --ids <every id in CO>    # -> child_only_blob
   ```
   Find the local YAML/JSON config — commonly `config.yaml`/`config.json` at the repo root, with
   `tracing.enabled_blob` and `tracing.child_only_blob`. **If it isn't obviously the right
   file/fields, ask the user.** Set both (for a disable request, both `""`). This replaces
   what's there; to _add_ to what's on, include the previously-enabled ids in `E` too.

   **Then re-read both fields and compare them character-for-character against what `encode`
   printed.** Don't report success until they match exactly. A value off by even one character
   is invalid and silently leaves tracing unchanged (a real failure mode: one dropped character
   makes the blob fail to decode, and the app keeps its old state with no visible effect).

7. **Report tersely.** Just confirm tracing was turned on/off for the requested domain, or that
   it failed and why. Nothing else — no function list, no mechanism explanation, no unsolicited
   follow-up.
