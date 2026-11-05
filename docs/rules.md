# Writing rules

Fino rules are TOML data, not executable plugins. Start with
`fino skills rules` for the agent workflow and `fino schema rule` for the exact
fields supported by your installed binary. See [configuration](configuration.md)
for project setup.

## Author and verify

1. State what should fail and the closest case that should pass.
2. Store one rule in `<rules_dir>/<id>.toml`. The filename must match its
   lowercase kebab-case `id`. Supply an actionable `message`; use `description`
   for the reason and suggested fix. Severity defaults to `error`; `warning`
   reports findings without failing checks.
3. Choose exactly one matcher:

   | Matcher            | Use for        | Finding location             |
   | ------------------ | -------------- | ---------------------------- |
   | `query`            | Elixir syntax  | Each `@finding` capture      |
   | `regex`            | Source text    | Each matched substring       |
   | `path_only = true` | File placement | Line 1 of each selected file |

4. Add embedded examples, then validate and test before scanning real code:

   ```sh
   fino validate .fino/rules/no-inspect.toml
   fino test .fino/rules/no-inspect.toml
   fino check lib/example.ex --json
   fino validate
   fino test
   ```

Selected rule paths refer to rule files, not source files. Use `--root` and
`--config` for a project outside the current directory.

## Syntax queries

Inspect the parser's actual tree before writing a query:

```sh
fino tree --code 'IO.inspect(value)'
fino tree lib/example.ex
```

```toml
id = "no-inspect"
message = "Remove the debug inspection."
description = "Debug output can expose values and clutter logs."
query = '''
((dot left: (alias) @_module right: (identifier) @finding)
 (#eq? @_module "IO") (#eq? @finding "inspect"))
'''

[[tests]]
name = "reports inspection"
code = "IO.inspect(value)\n"
expect = [1]

[[tests]]
name = "ignores text in strings"
code = '"IO.inspect(value)"'
expect = []
```

Capture the smallest useful span. Helper captures should start with `_`.
Pipes, calls without parentheses, and function captures have different tree
shapes: test each relevant form. Queries match syntax, not resolved names or
types; the query above does not resolve `alias IO, as: Output`.

Standard tree-sitter text predicates are supported, along with `#count-eq?`,
`#count-gt?`, and `#count-lt?` for named child counts excluding comments.
Use [scoped matching](query-stages.md) for per-capture `conditions`, ordered
root-rebased `stages`, and source-text `comparisons`. These are not semantic
analysis or arbitrary AST normalization.

## Text and path matchers

Regex uses Rust syntax with look-around support. Literal TOML strings (`'...'`
or `'''...'''`) preserve backslashes. `scope = "line"` matches each physical
line separately; the default matches the whole file.

`mask` blanks or removes selected syntax before matching. Choose the policy
explicitly: a whitespace check may intentionally ignore literal content but
still check comments. `prefilter` is an optional fast regex on **raw** text,
before masking. It must accept every true match; a bad prefilter silently loses
findings. `match_length` bounds the length of a masked match in bytes, Unicode
scalars (default), or grapheme clusters; consult the rule schema for its shape.

For file placement, combine `path_only = true` with `include`/`exclude` or
`if_path = { regex = '...' }`. Rule filters narrow project selection; they
cannot include files excluded by the project.

## Filters and examples

| Field                           | Effect                                     |
| ------------------------------- | ------------------------------------------ |
| `include`, `exclude`, `if_path` | Narrow eligible root-relative paths        |
| `if_file`, `unless_file`        | Gate the whole file using regex or query   |
| `skip_lines`                    | Drop findings on selected lines            |
| `skip_inside`                   | Drop findings inside captured syntax nodes |
| `limit = "first-per-file"`      | Keep the first match before suppression    |

Each `[[tests]]` needs `name`, valid Elixir `code`, and `expect`: the one-based
line of every finding, repeated for multiple findings on one line. `expect = []`
asserts no findings. `path` defaults to `lib/example.ex`; set it explicitly for
path-filter tests. Cover violations, clean code, near misses, and relevant
comments, literals, multiline forms, pipes, captures, and exclusions.

Avoid broad descendant queries and unbounded regex work when a specific pattern
will do. Compare `fino profile --runs 5` on unchanged files before and after an
optimization. Profiling does not fail on findings or change snapshots.

## Runnable examples

The [example policy](../examples/rules/.fino/config.toml) contains:

- [io-inspect](../examples/rules/.fino/rules/io-inspect.toml): syntax queries,
  argument counts, and a Credo suppression alias.
- [iex-pry](../examples/rules/.fino/rules/iex-pry.toml): a small structural query
  with call, capture, comment, and string examples.
- [trailing-whitespace](../examples/rules/.fino/rules/trailing-whitespace.toml):
  line regex, raw-text prefilter, and literal masking.
- [test-file-location](../examples/rules/.fino/rules/test-file-location.toml):
  path-only matching with explicit test paths.

Run from the repository root:

```sh
fino validate --root examples/rules
fino test --root examples/rules
```

These are generic, opt-in examples, not a bundled default policy. The debug
checks adapt generic Credo built-in migrations; they do not promise complete
Credo parity. In particular, `io-inspect` counts explicit arguments (pipe input
is not an explicit argument), and none of these queries resolves aliases.
The whitespace example checks ASCII spaces/tabs and ignores all supported
literal kinds, rather than reproducing Credo's precise whitespace policy.
