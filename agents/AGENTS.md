# Configuring stylus tracing in this repo

This application uses [`stylus`](https://github.com/samugi/stylus) for dynamic, per-function
tracing: any `#[traceable]` function can be turned on/off at runtime by editing a local config
file. When asked to change what's traced (e.g. "trace the database", "turn off tracing"),
follow this exact procedure. Don't write a new script and don't improvise a different
mechanism — everything needed is `stylus-cli encode` plus editing one file.

## 1. One-time setup: `stylus-cli`

Check first: `stylus-cli --version`. If it's not installed:

```
cargo install --git git@github.com:samugi/stylus.git stylus-cli
```

This is the only tool you need. It only does one thing (`encode`) and needs no other setup.

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

## 4. Generate the blob

```
stylus-cli encode --ids <id1> <id2> ...
```

(or `stylus-cli encode --names <name1> <name2> ...` if working from names instead of ids)

This prints a single blob string to stdout.

## 5. Update the local config

Find the local YAML/JSON config file holding the enabled blob — commonly `config.yaml` or
`config.json` at the repo root, under a key like `tracing.enabled_blob`. **If it isn't
obviously the right file/field, ask the user which one to update — don't guess.**

Set that field to the blob from step 4 and save the file. This replaces whatever was
previously enabled. If asked to *add* to what's currently on, include the previously-enabled
functions in this selection too.

To turn tracing off entirely, set the field to an empty string instead of generating a blob.

## 6. Report

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
