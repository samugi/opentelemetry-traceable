---
name: configure-tracing
description: Turn stylus tracing on or off for a subset of this app's functions, described in plain English (e.g. "trace the database", "trace the payment gateway", "turn off tracing"). Manually invoked -- run explicitly when asked to change what's traced.
---

> **Written for:** `stylus` @ `main` (this repo has no tagged releases yet — `main` is the only
> version, and it can gain new flags at any time). This skill assumes `stylus-cli encode`
> supports `--names`, `--ids`, `--all`, and `--fp-rate`. If `stylus-cli encode --help` doesn't
> list one of those, this file or your installed `stylus-cli` is stale — re-fetch this file
> from https://github.com/samugi/stylus (`agents/SKILL.md`) and reinstall `stylus-cli` (step 1)
> before continuing.

This application uses `stylus` for dynamic, per-function tracing: any `#[traceable]` function
can be turned on/off at runtime by editing a local config file. `args` is the request in plain
English (e.g. "trace the database"). Follow this procedure step by step, in order, and don't
skip a step because it looks unnecessary. Do not write a new script and do not improvise a
different mechanism — everything needed is `stylus-cli encode`, `rg`, and editing one file.
Every step is a fixed, mechanical procedure (a lookup or a pattern match), not something to
reason about freely.

1. **Make sure `stylus-cli` has what this skill needs.** Run `stylus-cli encode --help` (if the
   command isn't found at all, skip straight to reinstalling below) and check its output lists
   all four of `--names`, `--ids`, `--all`, `--fp-rate`.
   - All four present: it's already good, do nothing else here.
   - Any missing, or the command isn't found: (re)install it:
     ```
     cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
     ```
   Checking first instead of always reinstalling is deliberate: reinstalling recompiles from
   source every time, which is slow and usually unnecessary. The check is what protects you
   from a stale binary (this repo has no version tags, so "already installed" alone doesn't
   mean "has the flag you need") — it's just cheaper than a blind reinstall. This is the only
   tool you need, and it only does one thing (`encode`).

   **Shortcut for "enable/disable everything":** if `args` asks for tracing to be turned on for
   everything, or off entirely, skip straight to it — no catalog lookup, no picking functions,
   no ancestor walk. Enable everything with `stylus-cli encode --all`, then go to step 6.
   Disable everything by setting the config field to an empty string in step 6 — no blob
   needed. Otherwise, continue below.

2. **Get the catalog.** You need a JSON file mapping every `#[traceable]` function in this app
   to an id. Expect it to already exist in this repo (common names: `stylus-catalog.json`,
   `catalog.json`, often at the repo root or in `docs/`; `grep -rl '"hash": "fnv1a64"' .` finds it
   if the name isn't obvious).

   **If you can't find one, stop and ask the user** where it is, or ask them to generate it.
   Don't try to generate it yourself — catalog dumping is specific to this app's own binary
   (`stylus-cli` can't do it: the registry only exists inside a compiled binary that actually
   has `#[traceable]` functions linked into it), so producing it is on the user, not you. If
   useful context, the command is normally `cargo run --quiet -- catalog > stylus-catalog.json`,
   but confirm rather than assume.

3. **Pick the functions matching `args`.** The catalog's `functions` array has `{name, id}`
   pairs.

   **Before doing any matching here or in step 4, compute each function's match key** — do
   this once, for the whole catalog, up front, and reuse it for both steps:
   - Start each function's key as just its own name's last `::`-separated segment (e.g.
     `insert` for `stylus_demo::domain::db::orders::insert`).
   - If any *other* function in the catalog shares that same last segment, the key is
     ambiguous — extend it by one more segment from the end (e.g. `orders::insert`) and check
     again. Keep adding segments until it's unique across the whole catalog.
   - Most functions need only their bare name; only ones with a same-named sibling elsewhere
     (a common pattern for CRUD-style names like `insert`/`send`/`find_by_id` reused across
     modules) need more segments. Matching on the bare last segment alone produces false
     positives whenever two functions share one (searching for plain `insert(` would wrongly
     credit a function with calling *every* `insert`, not just the one it actually calls) — the
     match key is exactly enough of the trailing path to tell them apart, and no more.

   **Module/concern request** ("trace the database"): match by name substring against the
   catalog (e.g. everything containing `::db::`). That's the whole step — no source reading.

   **Flow request** ("trace checkout end-to-end"): the entry point plus everything it calls,
   found by mechanical search, not by reading and understanding the code. Repeat for each
   function in a work list, starting with just the entry point, until a full pass adds nothing
   new:
   1. `rg -n "fn <short_name>"` (the function's own last path segment) to find where it's
      defined.
   2. Read only that function's body: from the opening `{` to its matching closing `}`. Don't
      read anything else in the file.
   3. For every *other* function in the catalog, check whether `<its match key>(` (not just its
      bare name) appears as a substring of the body text from step 2.
   4. Every match is a function this one calls: add its full catalog name to the work list (if
      not already picked) and to the selection.
   5. Repeat from step 1 for each newly added function.

4. **Include enabled ancestors, to keep the trace hierarchy intact.** For every function
   selected in step 3 that is **not** marked `"child_only": true` in the catalog, find whatever
   calls it, by mechanical search, not by reading and understanding the code. This only applies
   when *enabling*; skip this step entirely for disable requests, since disabling a function
   doesn't break hierarchy the same way.

   **Skip any `child_only` function here — do not walk its callers.** A `child_only` function
   already only creates a span when some ancestor is actively being traced (that's the entire
   point of the flag), so it needs no ancestor added on its behalf. Walking its callers anyway
   would pull in *every* unrelated flow that happens to also call it — e.g. if
   `analytics::track_event` is `child_only` and called by all 8 scenarios, walking its
   ancestors for a "trace checkout" request would incorrectly enable the other 7 scenarios too,
   defeating the whole point of a scoped request. Only non-`child_only` functions in the
   selection can actually end up as orphan roots, so they're the only ones that need this step.

   Starting from the non-`child_only` functions in the step-3 selection as a work list, repeat
   until a full pass adds nothing new (and don't add a `child_only` function's own callers to
   the work list either, if one gets pulled in some other way):
   1. Take one function's match key (from step 3 — not just its bare name, to avoid matching
      some *other* function's call site by mistake) and run `rg -n "<match key>\("` across the
      whole source tree to find every occurrence.
   2. Discard any occurrence that's the function's *own definition*, not a call to it —
      recognize these because `fn` appears immediately before the name (e.g.
      `pub async fn track_event(`). For every remaining occurrence (a file and line number),
      scan upward through that same file, line by line, for the nearest line above it matching
      `fn \w+` (with an optional `pub`/`async` before it) — that is the enclosing (calling)
      function's definition.
   3. Take that enclosing function's own last path segment and look it up in the catalog. If
      exactly one catalog entry ends with that segment, it's traceable: add its full catalog name
      to the work list (if not already picked) and to the selection. If *more than one* catalog
      entry shares that segment (rare for calling functions, common for called ones), pick the
      one whose full name's module path matches the file/directory you found it in (e.g. a
      function found in `src/domain/db.rs` most likely belongs to a catalog entry containing
      `::db::`); if it's still ambiguous, ask the user rather than guessing.
   4. Repeat from step 1 for the next function in the work list.

   Why this matters: disabling a function doesn't just skip its own span, it also leaves the
   ambient trace context untouched (documented `stylus` behavior). So if a traced function's
   real caller isn't *also* traced, the child's span shows up as a disconnected root span
   instead of properly nested under its actual caller — you lose the ability to see where the
   call came from. Including the full traceable ancestor chain avoids that without merging or
   splitting any spans: each function still gets exactly its own span, just correctly parented.

   Check the catalog's `child_only` field while you do this: a function with
   `"child_only": true` *only* creates a span when called from within an already-active span
   — never as a root, even when its own flag is enabled. For these, the ancestor walk isn't
   optional: enable a `child_only` function without an enabled ancestor above it and it will
   silently produce no span at all.

5. **Generate the blob**:

   ```
   stylus-cli encode --ids <id1> <id2> ...
   ```

   (or `--names <name1> ...` if working from names instead of ids) — using the full selection
   from steps 3 and 4 combined. This prints a single blob string to stdout — keep it exactly as
   printed for the next step, don't retype it.

6. **Update the local config, then verify the write.** Find the local YAML/JSON config file
   holding the enabled blob — commonly `config.yaml`/`config.json` at the repo root, under a
   key like `tracing.enabled_blob`. **If it isn't obviously the right file/field, ask the user
   which one to update.** Set that field to the blob from step 5 (or an empty string, if
   disabling) and save. This replaces whatever was previously enabled — if asked to _add_ to
   what's currently on, include the previously-enabled functions in this selection too.

   **Then re-read the field you just wrote and compare it, character for character, against the
   blob step 5 printed.** Don't report success until they match exactly. If they don't, fix the
   file and check again — a value off by even one character is invalid and silently leaves
   tracing unchanged (a real, previously-seen failure mode: a single dropped character produces
   a blob that fails to decode, and the app leaves tracing exactly as it was, with no visible
   effect at all).

7. **Report tersely.** Just confirm tracing was turned on/off for the requested domain, or that
   it failed and why. Nothing else — no function list, no mechanism explanation, no unsolicited
   follow-up.
