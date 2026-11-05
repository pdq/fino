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
fino profile [FILES...] [--files-from FILE] [--runs N] [--threads N] [--json]
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

### Rule cost profiling

`fino profile` ranks every loaded rule by median elapsed milliseconds, expensive
first. It performs one warmup followed by `--runs N` measured scans (default **3**;
must be positive). The default is **one worker thread** for easier comparison;
`--threads N`, `--root`, `--config`, file arguments, and `--files-from` work as usual.

```sh
fino profile --runs 5
fino profile lib/example.ex --threads 2 --json
```

This is **independent rule-query evaluation; query sharing disabled**. Each scan
parses each selected file once, then evaluates the loaded rules against that tree.
Rules are not profiled by rescanning the repository separately for each rule.
Compilation and warmup are excluded from measured samples. The table includes
each rule's median cumulative elapsed time and findings from the last completed
scan, plus separate overall scan wall time and cumulative parsing medians.
Files, loaded rules, and total findings also describe the last completed scan.

Timings are elapsed durations, **not CPU time or additive wall-time attribution**.
Workers overlap when multithreading is enabled. Medians need not add up. Removing
a rule from normal linting does not necessarily save its independent profile cost:
`check --timings` uses shared query traversals and reports that traversal separately,
while `profile` disables query sharing to give query rules useful individual costs.
Per-rule timers cover rule matching/masking, not file-level eligibility checks,
file reading, suppression processing, or all reporting overhead. Zero means no
recorded matching time (for example, excluded or disabled), not proof of zero work.
The current engine exposes durations, not evaluated-file counts. Files are reread
on every scan; keep the working tree stable while measuring.

Profiling never loads, adopts, prunes, or writes snapshots. Findings do not fail
this diagnostic: exit **0** means profiling completed, even with lint violations.
Invalid policy, selected-file read errors, parse errors, and other scan errors
stop profiling and exit **2**; partial samples must not be treated as a complete run.

`profile --json` emits one diagnostic object with `schema_version = 1`, mode and
attribution notes, requested/completed runs, warmup status, threads, counts, raw
scan/parsing samples and medians, ranked `rules` (id, samples, median, findings,
and timing status), `errors`, and `warnings`. Operational failures use this same
object; argument-parse errors use stderr as usual. Presentation options are ignored:
no color, snippets, or grouped headers. Ordinary `check --json` is unchanged.

### Human diagnostics

`check` and `snapshot` show clickable `file:line:column` locations, severity,
rule ids, and source snippets. Choose the presentation without changing lint results:

```sh
fino check --color auto --context 2       # defaults; color only on a terminal
fino check --group-by rule               # rule headers with descriptions
fino check --group-by none --context 0   # default ordering, compact findings
fino check --color always                # force ANSI color, including in pipes
fino check --color never                 # plain text
```

The result summary includes the number of loaded rules (`N rules`), including
rules whose path filters skip some or all selected files. It is not a count of
rule-file evaluations. JSON reports keep their existing shape.

`--color auto` respects `NO_COLOR` (when present, even if empty) and checks stdout
and stderr separately. `always` overrides `NO_COLOR`. Grouping is opt-in, sorts
rule ids, and shows each description once, falling back to the rule message.
`--context N` adds N surrounding lines on each side of a match; `0` hides snippets.
Multiline matches mark each matched line. Source is cached per render; an unreadable
or changed source file can omit its snippet without changing the result or exit code.

Coordinates remain the engine's one-based byte columns. Snippets expand tabs to
four-column stops measured from the start of the source, excluding the line-number
gutter. Printable Unicode stays literal. Carets use conventional non-CJK terminal
cell widths for whole strings, including combining marks and emoji sequences;
ambiguous-width characters are narrow. Font or terminal-specific rendering can differ.
Controls, bidi direction markers, and hidden formatting controls are escaped rather
than executed; normal script joiners and emoji joiners remain literal. `--json`
ignores all three presentation options: no colors, snippets, headers, or changes
to its report shape.

### Shell completion

Fino uses [Usage](https://usage.jdx.dev/) for shell completion. Install the Usage
CLI with `mise use -g usage` (or `brew install usage`), and put both `fino` and
`usage` on `PATH`. Run the command for your shell:

```sh
usage generate completion zsh fino --usage-cmd 'fino --usage-spec' --install
usage generate completion bash fino --usage-cmd 'fino --usage-spec' --install
usage generate completion fish fino --usage-cmd 'fino --usage-spec' --install
```

Follow the setup instructions printed by Usage, then open a new shell. `--install`
writes the completion file; it does not change your shell startup files. Bash
also needs bash-completion 2.11 or later installed and sourced. Keep `usage` on
`PATH`: the generated completion calls it at runtime.

`fino --usage-spec` prints the KDL spec generated from the same command definition
that parses arguments and renders help. Completion reads it from the installed
binary, so there is no separate spec to keep in sync. Use `fino COMMAND --help`
for command options. To save the spec for other Usage tools:

```sh
fino --usage-spec > fino.usage.kdl
```

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
