---
name: fino-rules
description: Create, test, debug, and optimize declarative Fino lint rules in TOML for Elixir. Use when editing rule files, writing tree-sitter queries or regex matchers, fixing false positives, or migrating a Credo check to Fino. For project setup, file selection, suppression configuration, and snapshots, use fino-config instead.
---

# Fino rules

1. Read the project's config (normally `.fino/config.toml`) and nearby rules. Run
   `fino schema rule` for the installed version's fields. Use TOML, not YAML; do
   not invent plugin APIs or evaluate application code.
2. Define the violation and its nearest allowed case before writing a matcher.
   Choose exactly one: `query` for syntax, `regex` for text, or `path_only = true`
   for file placement. Name the file `<id>.toml` with a lowercase kebab-case id.
   Write an actionable `message` and a short `description` explaining the fix.
3. For queries, inspect real syntax with `fino tree --code 'IO.inspect(value)'`
   or `fino tree path/to/file.ex`. Never guess node kinds or fields. Capture the
   smallest useful diagnostic span as `@finding`; every such capture reports.
   Use `_`-prefixed captures for helpers. Match concrete syntax: pipes, captures,
   aliases, and parenthesized calls can differ. Queries do not resolve module aliases.
4. For regex, choose file scope or `scope = "line"`. Use literal TOML strings to
   preserve backslashes. Mask strings/comments only if the policy excludes them.
   A `prefilter` sees raw text and must never reject a true masked-text match.
   Do not add one without testing that invariant.
5. Add `[[tests]]` with `name`, `code`, and `expect` (one-based finding lines;
   repeat a line for multiple findings). Add positive, negative, and near-miss
   cases; exercise comments, strings, multiline syntax, pipes, and captures when
   relevant. Set `path` explicitly to test rule path filters; the default is
   `lib/example.ex`. Keep snippets valid Elixir.
6. Run from the project root, substituting the actual rule path:

   ```sh
   fino validate .fino/rules/no-inspect.toml
   fino test .fino/rules/no-inspect.toml
   fino check lib/example.ex --json
   ```

   Then run `fino validate` and `fino test` for the whole policy. Fix unexpected
   findings rather than weakening assertions, exclusions, or severity to pass.
   Use `--root PATH` and `--config FILE` when needed.
7. For expensive rules, measure `fino profile --runs 5` before and after on the
   same unchanged files. Prefer narrow structural patterns over broad descendant
   searches. Use `conditions` for checks scoped to one capture, `stages` for
   root matching with explicit singleton unwrapping, and `comparisons` for source
   text equality only when a simpler query cannot express the policy. Consult
   https://github.com/pdqcom/fino/blob/main/docs/query-stages.md for semantics.

## Minimal query rule

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

For a Credo migration, declare `[compat] credo = "Credo.Check.Warning.IoInspect"`
only when that identity is intentional. Test behavior rather than assuming parity.
Do not adopt snapshots or copy private application checks without permission.
See https://github.com/pdqcom/fino/blob/main/docs/rules.md for the rule guide
and runnable, generic examples.
