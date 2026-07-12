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
follow this exact procedure. Don't write a new script and don't improvise a different
mechanism — everything needed is `stylus-cli encode` plus editing one file.

## 1. One-time setup: `stylus-cli`

Because `main` is the only version and it moves, don't just check whether `stylus-cli` is
already installed and skip if so — that can leave you silently stuck with a stale build missing
newer flags (like `--all`). Always (re)install to make sure you have the latest:

```
cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
```

This is the only tool you need. It only does one thing (`encode`) and needs no other setup.

## Shortcut: enabling or disabling *everything*

If the request is to trace everything, or to turn tracing off entirely, skip straight to it —
no schema lookup, no picking functions, no ancestor walk:

- **Enable everything**: `stylus-cli encode --all`, then put the result in the config (step 6).
- **Disable everything**: set the config field to an empty string (step 6) — no blob needed.

Otherwise, continue below.

## 2. Get the schema

You need a JSON file mapping every `#[traceable]` function in this app to an id — this is what
you'll search to figure out which functions match the request. **Expect it to already exist**
in this repo (common names: `stylus-schema.json`, `schema.json`; often at the repo root or in
`docs/`). A quick `grep -rl '"hash": "fnv1a64"' .` will find it if the name isn't obvious.

**If you can't find one, stop and ask the user** where it is, or ask them to generate it. Don't
try to generate it yourself: schema dumping is specific to this app's own binary (`stylus-cli`
cannot do it — see the note below if you want to know why), so this is on the user, not you.
If it's useful context: the command is normally `cargo run --quiet -- schema > stylus-schema.json`,
but that's this repo's convention to confirm, not yours to assume and run.

## 3. Pick the functions matching the request

The schema JSON has a `functions` array of `{"name": ..., "id": ...}` pairs. For a request
naming a module/concern (e.g. "trace the database"), match by name substring (e.g. everything
containing `::db::`). For a request describing a flow (e.g. "trace checkout end-to-end"), read
the relevant source to find every function actually called along that path — a name/module
filter alone won't capture that.

## 4. Include enabled ancestors, to keep the trace hierarchy intact

For every function selected in step 3, also find whatever calls it — directly or transitively,
all the way up to wherever that chain starts (a top-level flow/entry point, another module,
etc.) — by reading the source. If a caller is itself `#[traceable]`, add it to the selection
too, and repeat for *its* callers, and so on up the chain.

Why this matters: disabling a function doesn't just skip its own span, it also leaves the
ambient trace context untouched (that's documented `stylus` behavior). So if a traced
function's real caller isn't *also* traced, the child's span shows up as a disconnected root
span instead of properly nested under its actual caller — you lose the ability to see where
the call came from. Including the full traceable ancestor chain avoids that without merging or
splitting any spans: each function still gets exactly its own span, just correctly parented.

This only applies when *enabling* — disabling a function doesn't break hierarchy the same way,
so no ancestor walk is needed for disable requests.

## 5. Generate the blob

```
stylus-cli encode --ids <id1> <id2> ...
```

(or `stylus-cli encode --names <name1> <name2> ...` if working from names instead of ids) —
using the full selection from steps 3 and 4 combined.

This prints a single blob string to stdout.

## 6. Update the local config

Find the local YAML/JSON config file holding the enabled blob — commonly `config.yaml` or
`config.json` at the repo root, under a key like `tracing.enabled_blob`. **If it isn't
obviously the right file/field, ask the user which one to update — don't guess.**

Set that field to the blob from step 5 and save the file. This replaces whatever was
previously enabled. If asked to *add* to what's currently on, include the previously-enabled
functions in this selection too.

To turn tracing off entirely, set the field to an empty string instead of generating a blob.

## 7. Report

Just confirm: tracing was turned on/off for the requested domain, or that it failed and why.
Nothing else — no list of every function enabled, no explanation of the mechanism, no
unsolicited follow-up.

---

<details>
<summary>Why can't <code>stylus-cli</code> just produce the schema itself?</summary>

The `{name, id}` registry only exists inside the memory of a specific compiled binary that has
`#[traceable]` functions linked into it (via a linker-section trick). `stylus-cli` is a
separate, generic binary with none of that app's code in it — there's no way for it to reach
into a different binary's internal registry. Only the application's own binary can produce its
schema.

</details>
