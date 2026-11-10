# Project configuration

Create `.fino/config.toml` and place each rule in `.fino/rules/<id>.toml`:

```toml
include = ["**/*.ex", "**/*.exs"]
exclude = ["**/generated/**"]
rules_dir = "rules"
snapshot_file = "snapshot.jsonl"
```

These are the default include patterns, rule directory, and snapshot filename;
`exclude` defaults to empty. Unknown fields fail validation. All rule files in
the configured directory are loaded; there is no enabled-rule list.

## Paths and selection

The scan root defaults to the current directory. `--root PATH` changes it;
`--config FILE` selects a different config file. Globs and explicit source paths
are relative to the scan root. `rules_dir` and `snapshot_file` are relative to
the config file. Rule-level filters further narrow the project file set.

Full scans respect ignore files and skip build/dependency directories such as
`_build`, `deps`, `node_modules`, and `target`. Explicit paths still obey project
filters and must remain inside the scan root; unsupported/excluded files are
skipped. Only Elixir `.ex` and `.exs` files are currently linted.

```sh
fino validate
fino test
fino check
fino check lib/example.ex test/example_test.exs
fino check --files-from changed-files.txt --json
```

Manifest files contain one root-relative path per line. Changed-file checks are
useful in hooks; run a full check in CI. Exit codes are **0** pass, **1** new
error findings or failed examples, and **2** invalid policy, read errors, or
parse errors. `check` never writes policy or snapshots. A check with no loaded
rules fails rather than silently passing.

## Suppressions and migration

Prefer fixing findings; keep exceptions targeted and explain them:

```elixir
# fino:disable-next-line io-inspect -- temporary investigation
IO.inspect(value)
```

`disable-line` and `disable-file` are also supported. Use a rule id or `all`.
Unknown directives and rule ids fail closed.

For existing Credo suppression comments, enable this in config:

```toml
[compat]
credo_directives = true
```

Declare the corresponding identity in each migrated rule:

```toml
[compat]
credo = "Credo.Check.Warning.IoInspect"
```

This enables suppression compatibility, not automatic translation of Credo
checks or semantic parity. For other comment formats, inspect
`fino schema config` for `suppression_dialects` and `fino schema rule` for
`compat.suppressions`. Custom dialect regexes must capture a nonempty `targets`
group. Leave raw-text scanning off unless legacy compatibility requires it.

## Snapshots

With explicit agreement to accept existing violations, `fino snapshot` adopts
current **error** findings into a committable JSON Lines file. Warnings do not
enter snapshots. Commit the policy and snapshot together; do not edit snapshot
records by hand or create a baseline just to silence new findings.

`fino snapshot --update` removes stale entries without accepting new violations.
Subset runs affect only selected files; full scans can prune deleted files.
Read or parse errors prevent writes. `check` reports new findings and stale
entries without modifying the snapshot. See [violation snapshots](../README.md#violation-snapshots)
for identity and concurrency details.

## Agent skills

Discover the portable guides in CLI help, then print the one you need:

```sh
fino skills
fino skills rules
fino skills config
fino skills --json
```

Guides are embedded in the binary and need no project configuration or network.
JSON output is an array of `{ name, topic, content }` objects; a topic selects
one object. The content is the exact distributable `SKILL.md`, including metadata.

Install through Vercel's Skills CLI:

```sh
aubx skills add pdqcom/fino --list
aubx skills add pdqcom/fino --skill fino-rules fino-config
```

Use `--global` for user-wide installation. Before publication, run
`aubx skills add . --list` from a checkout to discover the local skills.
The standard `skills/<name>/SKILL.md` layout needs no custom installer.
Installed guides use the binary's schemas as the authority for version-specific
fields. See [writing rules](rules.md) for the human guide and tested examples.
