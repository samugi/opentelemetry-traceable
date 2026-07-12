---
name: configure-tracing
description: Turn stylus tracing on or off for a subset of this app's functions, described in plain English (e.g. "trace the database", "trace the payment gateway", "turn off tracing"). Manually invoked -- run explicitly when asked to change what's traced.
---

This application uses `stylus` for dynamic, per-function tracing: any `#[traceable]` function
can be turned on/off at runtime by editing a local config file. `args` is the request in plain
English (e.g. "trace the database"). Follow this procedure exactly. Do not write a new script
and do not improvise a different mechanism — everything needed is `stylus-cli encode` plus
editing one file.

1. **Make sure `stylus-cli` is installed.** Check `stylus-cli --version` first. If missing:
   ```
   cargo install --git git@github.com:samugi/stylus.git stylus-cli
   ```
   This is the only tool you need, and it only does one thing (`encode`).

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

4. **Generate the blob**:
   ```
   stylus-cli encode --ids <id1> <id2> ...
   ```
   (or `--names <name1> ...` if working from names instead of ids)

5. **Update the local config.** Find the local YAML/JSON config file holding the enabled blob
   — commonly `config.yaml`/`config.json` at the repo root, under a key like
   `tracing.enabled_blob`. **If it isn't obviously the right file/field, ask the user which one
   to update.** Set that field to the blob from step 4 and save. This replaces whatever was
   previously enabled — if asked to *add* to what's currently on, include the
   previously-enabled functions in this selection too. To disable tracing entirely, set the
   field to an empty string instead of generating a blob.

6. **Report tersely.** Just confirm tracing was turned on/off for the requested domain, or that
   it failed and why. Nothing else — no function list, no mechanism explanation, no unsolicited
   follow-up.
