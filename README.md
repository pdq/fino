# fino

**Refine your code, one rule at a time.**

Named after Spanish _fino_: fine or refined.

Fino is a declarative, parallel linter. Rules are data, not plugins: tree-sitter
queries, regular expressions, and path filters. No application code is evaluated.

**Early development:** Elixir is the first supported language. A language registry,
additional languages, and YAML rules are planned. Today, config and rules use TOML.

## Build

Install [mise](https://mise.jdx.dev/) and a C compiler for the parser runtime.
Tool versions and artifact digests are pinned in `mise.toml` and `mise.lock`.

```sh
mise install
mise run build:release
./target/release/fino --help
```

## A project policy

Put configuration in `.fino/config.toml`:

```toml
include = ["**/*.ex", "**/*.exs"]
exclude = ["**/generated/**"]
rules_dir = "rules"
snapshot_file = "snapshot.jsonl"
```

Each rule has its own file, such as `.fino/rules/no-inspect.toml`:

```toml
id = "no-inspect"
message = "Remove the debug inspection."
query = '''
((dot left: (alias) @_module right: (identifier) @finding)
 (#eq? @_module "IO") (#eq? @finding "inspect"))
'''

[[tests]]
name = "reports inspection"
code = "IO.inspect(value)\n"
expect = [1]

[[tests]]
name = "allows normal output"
code = "IO.puts(value)\n"
expect = []
```

Try the complete example in `examples/project/`.

## Commands

```sh
fino check [FILES...] [--files-from FILE] [--json] [--timings]
fino snapshot [FILES...]          # explicitly adopt current violations
fino snapshot --update [FILES...] # remove stale entries; never adopt new violations
fino validate [RULE_FILES...]
fino test [RULE_FILES...]
fino tree FILE
fino tree --code 'IO.inspect(x)'
fino schema rule                 # also: config
```

Exit codes: **0** pass, **1** new violations or failed examples, **2** invalid policy,
unreadable selected files, or parse errors. `check` never writes files. Explicit
file lists skip unsupported and excluded files, making them suitable for hooks.
Paths must be relative to `--root` (default `.`).

### Rule fields

Choose exactly one matcher: `query`, `regex`, or `path_only = true`.

| Field                        | Purpose                                                           |
| ---------------------------- | ----------------------------------------------------------------- |
| `include` / `exclude`        | Select paths for a rule                                           |
| `scope = "line"`             | Apply a regex per line                                            |
| `mask`                       | Blank or remove strings, comments, or query-selected nodes        |
| `match_length`               | Inclusive masked-match length bounds: bytes, scalars or graphemes |
| `prefilter`                  | Cheap regex on raw text before expensive matching                 |
| `skip_lines` / `skip_inside` | Ignore selected lines or nodes                                    |
| `if_file` / `unless_file`    | File-level regex or query conditions                              |
| `stages`                     | Root-rebased queries with explicit wrapper unwrapping             |
| `conditions`                 | Queries scoped to a captured node                                 |
| `comparisons`                | Source-text equality between exported bindings                    |
| `limit = "first-per-file"`   | Report only the first match                                       |
| `severity = "warning"`       | Report without failing checks                                     |

Each `@finding` capture produces a finding. Queries support tree-sitter text
predicates and `#count-eq?`, `#count-gt?`, `#count-lt?` for named children, excluding
comments. Rule filenames must match their ids. Validate and test before enabling.
Generated JSON Schemas in `schema/` provide editor validation and completion.
See [scoped matching](docs/query-stages.md) for stage exports, conditions, and resource limits.

### Suppression

```elixir
# fino:disable-next-line no-inspect -- temporary investigation
IO.inspect(value)
```

Also supported: `disable-line`, `disable-file`, and `all` in place of a rule id.
Unknown directives and rule ids fail closed. Optional Credo compatibility uses
`[compat] credo_directives = true` in config and `[compat] credo = "Check.Name"`
in a rule.

### Violation snapshots

`snapshot` explicitly adopts error findings into sorted JSON Lines. Commit that file
with your policy. Entries hold the rule, relative file, normalized source line, and
occurrence index—not line numbers. Moving lines does not create new violations.
Source edits do; extra identical occurrences do too. Source text itself is the
fingerprint, avoiding hash collisions.

`check` fails only on new error findings and reports stale entries. Warnings never
enter the snapshot. `snapshot --update` removes stale entries without adopting new
findings. Subset runs only update selected files. Full scans can prune deleted files;
ignored existing files and unloaded rules retain their entries. Read or parse errors
prevent writes. Writes are atomic and serialized; policy changes during scanning
require a retry. Ignore companion guard and temporary files.

## Development

```sh
mise run build
mise run test
mise run check
mise run fix
mise run secrets:history
```

`check` runs formatting checks, Clippy, tests, jactionlint, and a secret scan.
`fix` applies formatting through hk; secret findings need manual review, not silent
deletion. hk installs pre-commit, commit-msg, and pre-push hooks. Commits and PR titles
must use Conventional Commits. The native `committed` binary comes from the
`github:crate-ci/committed` mise backend.

Secret scans redact findings and disable live credential validation. Pre-push and CI
scan the complete Git history across all branches and tags. CI checks PR titles on
title edits as well as new commits. See [release setup](docs/releases.md) for the
repository-level settings that enforce required checks.

The vendored tree-sitter runtime retains its upstream license and includes a bounded
parser-stack patch. See [the patch note](vendor/tree-sitter/PATCH.md).
