# Configuring stylus tracing in this repo

> **Written for:** `stylus` @ `main` (this repo has no tagged releases yet, so `main` is the
> only version — it can gain new flags at any time). This file assumes `stylus-cli encode`
> supports `--names`, `--ids`, `--all`, and `--fp-rate`. If `stylus-cli encode --help` doesn't
> list one of those, your local copy of this file or your installed `stylus-cli` is stale —
> re-fetch this file from https://github.com/samugi/stylus (`agents/AGENTS.md`) and reinstall
> `stylus-cli` (see step 1) before continuing.

This application uses [`stylus`](https://github.com/samugi/stylus) for dynamic, per-function
tracing: any `#[traceable]` function can be turned on/off at runtime by editing a local config
file. When asked to change what's traced (e.g. "trace the database", "turn off tracing"),
follow this exact procedure step by step. Don't write a new script, don't improvise a different
mechanism, and don't skip a step because it looks unnecessary — everything needed is
`stylus-cli encode`, `rg`, and editing one file. Every step below is a fixed, mechanical
procedure (a lookup or a pattern match), not something to reason about freely — follow it
literally, in order.

## 1. Make sure `stylus-cli` has what this file needs

Run `stylus-cli encode --help` (if the command isn't found at all, skip straight to
reinstalling below). Check its output lists all four of: `--names`, `--ids`, `--all`,
`--fp-rate`.

- **All four present**: it's already good — do nothing else here.
- **Any missing, or the command isn't found**: (re)install it:
  ```
  cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
  ```

Checking first instead of always reinstalling is deliberate: reinstalling recompiles from
source every time, which is slow and usually unnecessary. The check is what protects you from a
stale binary (this repo has no version tags, so "already installed" alone doesn't mean "has the
flag you need") — it's just cheaper than a blind reinstall.

This is the only tool you need. It only does one thing (`encode`) and needs no other setup.

## Shortcut: enabling or disabling *everything*

If the request is to trace everything, or to turn tracing off entirely, skip straight to it —
no catalog lookup, no picking functions, no ancestor walk:

- **Enable everything**: `stylus-cli encode --all`, then put the result in the config (step 6).
- **Disable everything**: set the config field to an empty string (step 6) — no blob needed.

Otherwise, continue below.

## 2. Get the catalog

You need a JSON file mapping every `#[traceable]` function in this app to an id — this is what
you'll search to figure out which functions match the request. **Expect it to already exist**
in this repo (common names: `stylus-catalog.json`, `catalog.json`; often at the repo root or in
`docs/`). A quick `grep -rl '"hash": "fnv1a64"' .` will find it if the name isn't obvious.

**If you can't find one, stop and ask the user** where it is, or ask them to generate it. Don't
try to generate it yourself: catalog dumping is specific to this app's own binary (`stylus-cli`
cannot do it — see the note below if you want to know why), so this is on the user, not you.
If it's useful context: the command is normally `cargo run --quiet -- catalog > stylus-catalog.json`,
but that's this repo's convention to confirm, not yours to assume and run.

**Before doing any matching in steps 3-4, compute each function's match key.** Do this once,
for the whole catalog, up front — steps 3 and 4 both reuse it:

- For each function, its match key starts as just its own name's last `::`-separated segment
  (e.g. `insert` for `stylus_demo::domain::db::orders::insert`).
- If any *other* function in the catalog shares that same last segment, that key is ambiguous —
  extend it by one more segment from the end (e.g. `orders::insert`) and check again. Keep
  adding segments until the key is unique across the whole catalog (not shared by any other
  function's same-length trailing segments).
- Most functions need only their bare name. Only ones with a same-named sibling elsewhere (a
  common pattern for CRUD-style names like `insert`/`send`/`find_by_id` reused across modules)
  need more segments.

This matters: matching on the bare last segment alone produces false positives whenever two
different functions share one (e.g. `db::orders::insert` and `db::reviews::insert` both end in
`insert` — searching for plain `insert(` in a function's body would wrongly credit it with
calling *both*, even if it only calls one). The match key is exactly enough of the trailing
path to tell them apart, and no more.

## 3. Pick the functions matching the request

The catalog JSON has a `functions` array of `{"name": ..., "id": ...}` pairs.

**Module/concern request** (e.g. "trace the database"): match by name substring against the
catalog (e.g. everything containing `::db::`). This is the whole step — no source reading.

**Flow request** (e.g. "trace checkout end-to-end"): the entry point plus everything it calls,
found by mechanical search — not by reading and understanding the code. Repeat for each
function in a work list, starting with just the entry point, until nothing new turns up:

1. `rg -n "fn <short_name>"` (the function's own last path segment) to find where it's defined.
2. Read only that function's body: from the opening `{` to its matching closing `}`. Do not
   read anything else in the file.
3. For every *other* function in the catalog, check whether `<its match key>(` (the key you
   computed above, not just its bare name) appears as a substring of the body text from step 2.
4. Every match is a function this one calls: add its full catalog name to your work list (if not
   already picked) and to the selection.
5. Repeat from step 1 for each newly added function. Stop when a full pass adds nothing new.

## 4. Include enabled ancestors, to keep the trace hierarchy intact

For every function selected in step 3 that is **not** marked `"child_only": true` in the
catalog, find whatever calls it, by mechanical search — not by reading and understanding the
code. This only applies when *enabling*; skip this step entirely for disable requests, since
disabling a function doesn't break hierarchy the same way.

**Skip any `child_only` function here — do not walk its callers.** A `child_only` function
already only creates a span when some ancestor is actively being traced (that's the entire
point of the flag), so it needs no ancestor added on its behalf. Walking its callers anyway
would pull in *every* unrelated flow that happens to also call it — e.g. if `analytics::track_event`
is `child_only` and called by all 8 scenarios, walking its ancestors for a "trace checkout"
request would incorrectly enable the other 7 scenarios too, defeating the whole point of a
scoped request. Only non-`child_only` functions in your selection can actually end up as
orphan roots, so they're the only ones that need this step.

Starting from the non-`child_only` functions in your step-3 selection as the work list, repeat
until a full pass adds nothing new (and don't add a `child_only` function's own callers to the
work list either, if one gets pulled in some other way):

1. Take one function's match key (computed above — not just its bare name, to avoid matching
   some *other* function's call site by mistake) and run `rg -n "<match key>\("` across the
   whole source tree to find every occurrence.
2. Discard any occurrence that's the function's *own definition*, not a call to it — recognize
   these because `fn` appears immediately before the name (e.g. `pub async fn track_event(`).
   For every remaining occurrence (a file and line number), scan upward through that same
   file, line by line, for the nearest line above it matching `fn \w+` (with an optional
   `pub`/`async` before it) — that is the enclosing (calling) function's definition.
3. Take that enclosing function's own last path segment and look it up in the catalog. If
   exactly one catalog entry ends with that segment, it's traceable: add its full catalog name to
   your work list (if not already picked) and to the selection. If *more than one* catalog entry
   shares that segment (rare for calling functions, common for called ones), pick the one whose
   full name's module path matches the file/directory you found it in (e.g. a function found in
   `src/domain/db.rs` most likely belongs to a catalog entry containing `::db::`); if it's still
   ambiguous, ask the user rather than guessing.
4. Repeat from step 1 for the next function in the work list.

Why this matters: disabling a function doesn't just skip its own span, it also leaves the
ambient trace context untouched (that's documented `stylus` behavior). So if a traced
function's real caller isn't *also* traced, the child's span shows up as a disconnected root
span instead of properly nested under its actual caller — you lose the ability to see where
the call came from. Including the full traceable ancestor chain avoids that without merging or
splitting any spans: each function still gets exactly its own span, just correctly parented.

Check the catalog's `child_only` field while you do this: a function with `"child_only": true`
*only* creates a span when it's called from within an already-active span — never as a root,
even when its own flag is enabled. For these, the ancestor walk isn't optional decoration, it's
required: enable a `child_only` function without an enabled ancestor above it and it will
silently produce no span at all.

## 5. Generate the blob

```
stylus-cli encode --ids <id1> <id2> ...
```

(or `stylus-cli encode --names <name1> <name2> ...` if working from names instead of ids) —
using the full selection from steps 3 and 4 combined.

This prints a single blob string to stdout. Keep it exactly as printed for the next step —
don't retype it.

## 6. Update the local config, then verify the write

Find the local YAML/JSON config file holding the enabled blob — commonly `config.yaml` or
`config.json` at the repo root, under a key like `tracing.enabled_blob`. **If it isn't
obviously the right file/field, ask the user which one to update — don't guess.**

Set that field to the blob from step 5 (or an empty string, if disabling) and save the file.
This replaces whatever was previously enabled. If asked to *add* to what's currently on,
include the previously-enabled functions in this selection too.

**Then re-read the field you just wrote and compare it, character for character, against the
blob step 5 printed.** Don't report success until they match exactly. If they don't match, fix
the file and check again — a value that's off by even one character is invalid and silently
leaves tracing unchanged (this is a real, previously-seen failure mode: a single dropped
character produces a blob that fails to decode, and the app leaves tracing exactly as it was,
with no visible effect at all).

## 7. Report

Just confirm: tracing was turned on/off for the requested domain, or that it failed and why.
Nothing else — no list of every function enabled, no explanation of the mechanism, no
unsolicited follow-up.

---

<details>
<summary>Why can't <code>stylus-cli</code> just produce the catalog itself?</summary>

The `{name, id}` registry only exists inside the memory of a specific compiled binary that has
`#[traceable]` functions linked into it (via a linker-section trick). `stylus-cli` is a
separate, generic binary with none of that app's code in it — there's no way for it to reach
into a different binary's internal registry. Only the application's own binary can produce its
catalog.

</details>
