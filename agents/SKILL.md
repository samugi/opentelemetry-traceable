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
English (e.g. "trace the database"). Follow this procedure exactly. Do not write a new script
and do not improvise a different mechanism — everything needed is `stylus-cli encode` plus
editing one file.

1. **Make sure `stylus-cli` is installed and up to date.** Because `main` is the only version
   and it moves, don't just check whether `stylus-cli` is already installed and skip if so —
   that can leave you silently stuck with a stale build missing newer flags (like `--all`).
   Always (re)install:

   ```
   cargo install --git ssh://git@github.com/samugi/stylus.git stylus-cli --force
   ```

   This is the only tool you need, and it only does one thing (`encode`).

   **Shortcut for "enable/disable everything":** if `args` asks for tracing to be turned on for
   everything, or off entirely, skip straight to it — no schema lookup, no picking functions,
   no ancestor walk. Enable everything with `stylus-cli encode --all`, then go to step 6.
   Disable everything by setting the config field to an empty string in step 6 — no blob
   needed. Otherwise, continue below.

2. **Get the schema.** You need a JSON file mapping every `#[traceable]` function in this app
   to an id. Expect it to already exist in this repo (common names: `stylus-schema.json`,
   `schema.json`, often at the repo root or in `docs/`; `grep -rl '"hash": "fnv1a64"' .` finds it
   if the name isn't obvious).

   **If you can't find one, stop and ask the user** where it is, or ask them to generate it.
   Don't try to generate it yourself — schema dumping is specific to this app's own binary
   (`stylus-cli` can't do it: the registry only exists inside a compiled binary that actually
   has `#[traceable]` functions linked into it), so producing it is on the user, not you. If
   useful context, the command is normally `cargo run --quiet -- schema > stylus-schema.json`,
   but confirm rather than assume.

3. **Pick the functions matching `args`.** The schema's `functions` array has `{name, id}`
   pairs. For a module/concern request ("trace the database"), match by name substring (e.g.
   everything containing `::db::`). For a flow request ("trace checkout end-to-end"), read the
   relevant source to find every function actually called along that path — a name filter
   alone won't capture that.

4. **Include enabled ancestors, to keep the trace hierarchy intact.** For every function
   selected in step 3, also find whatever calls it — directly or transitively, all the way up
   to wherever that chain starts (a top-level flow/entry point, another module, etc.) — by
   reading the source. If a caller is itself `#[traceable]`, add it to the selection too, and
   repeat for *its* callers, and so on up the chain.

   Why this matters: disabling a function doesn't just skip its own span, it also leaves the
   ambient trace context untouched (documented `stylus` behavior). So if a traced function's
   real caller isn't *also* traced, the child's span shows up as a disconnected root span
   instead of properly nested under its actual caller — you lose the ability to see where the
   call came from. Including the full traceable ancestor chain avoids that without merging or
   splitting any spans: each function still gets exactly its own span, just correctly parented.

   This only applies when *enabling* — disabling a function doesn't break hierarchy the same
   way, so skip this step for disable requests.

   Check the schema's `child_only` field while you do this: a function with
   `"child_only": true` *only* creates a span when called from within an already-active span
   — never as a root, even when its own flag is enabled. For these, the ancestor walk isn't
   optional: enable a `child_only` function without an enabled ancestor above it and it will
   silently produce no span at all.

5. **Generate the blob**:

   ```
   stylus-cli encode --ids <id1> <id2> ...
   ```

   (or `--names <name1> ...` if working from names instead of ids) — using the full selection
   from steps 3 and 4 combined.

6. **Update the local config.** Find the local YAML/JSON config file holding the enabled blob
   — commonly `config.yaml`/`config.json` at the repo root, under a key like
   `tracing.enabled_blob`. **If it isn't obviously the right file/field, ask the user which one
   to update.** Set that field to the blob from step 5 and save. This replaces whatever was
   previously enabled — if asked to _add_ to what's currently on, include the
   previously-enabled functions in this selection too. To disable tracing entirely, set the
   field to an empty string instead of generating a blob.

7. **Report tersely.** Just confirm tracing was turned on/off for the requested domain, or that
   it failed and why. Nothing else — no function list, no mechanism explanation, no unsolicited
   follow-up.
