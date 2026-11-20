# Elixir-shaped patterns (experimental)

Write the code shape you want to find rather than a Tree-sitter query:

```toml
id = "identity-map"
message = "Remove the identity map."
pattern = "Enum.map(_, fn value -> value end)"
where = { value = "variable" }
report = "callee"
```

This matches direct calls, equivalent pipes, quoted function names and singleton
parentheses. Repeating `value` requires equal normalized syntax, not equal source
spelling. `where` restricts that capture to a variable: destructuring patterns,
literals and calls are not variables.

This is inspired by ExAST, not an implementation of its complete pattern language.
It runs entirely in Rust. Fino neither starts BEAM nor evaluates Elixir source.

## Captures and alternatives

- `_` and underscore-prefixed names match one expression without binding it.
- One `...` per sequence matches zero or more intervening arguments/items.
  Fixed prefix and suffix items are aligned directly; there is no backtracking.
- Other variable names capture expressions. Repeated names require equality.
- Module aliases, callees, operators and literals match their specified values.
- Elixir context constants such as `__MODULE__` remain literal identifiers.
- Use a list of patterns for alternatives. Identical finding ranges are reported
  once; different occurrences on the same line remain separate.

```toml
pattern = [
  "Enum.filter(_, fn value -> value != nil end)",
  "Enum.filter(_, fn value -> value !== nil end)",
  "Enum.filter(_, fn value -> !is_nil(value) end)",
]
where = { value = "variable" }
```

Use TOML literal strings, as above, to avoid TOML backslash escaping.

## Normalization and limits

The prototype supports call- and operator-rooted patterns with ordinary calls, pipes, operators,
anonymous functions, clauses, tuples, lists, keyword arguments, ordinary static
strings/charlists, atoms, empty maps and nonnegative integers up to `u128`.

```toml
pattern = "_ * 0"
report = "operator"
```

`where = { value = "atom" }` accepts static atoms (including booleans and
`nil`), but not strings or interpolated atoms.
`where = { value = "variable" }` checks the syntactic variable node, not
general binding/data-flow analysis: an unresolved bare identifier can also denote
a zero-arity call in Elixir. Rules that need binding proof require explicit scope
support; do not use this constraint as a purity guarantee.

- Pipes insert their input as the first call argument.
- Comments and singleton parentheses do not change structural matching.
- Standard `do` blocks and equivalent `do:` keyword arguments match the same way.
- Atom and quoted-name escapes are decoded centrally. Strings are not atoms.
- Identifier comparisons use Unicode NFC. Quoted contents retain their value.

No alias/import resolution or macro expansion occurs. Definitions, attributes,
nonempty/partial maps and structs, heredoc patterns and interpolated literal patterns are not
supported. Unsupported pattern syntax is a validation error, not a wildcard.

Wildcards can consume source outside that subset. Repeated-capture equality on
unsupported source syntax fails the file rather than guessing whether it is
equivalent. Both normalization and matching share a bounded work counter per
file. Excessive work or nesting fails the file, never returns partial findings.
Pattern scans also cap accumulated intermediate hits at 100,000 per file before
filtering and line suppression, independently of the final finding limit.

All pattern rules share one candidate query over the required root kinds. An
immutable, engine-owned module/callee/operator index routes each candidate only
to relevant rules. File scans borrow route slices; they do not rebuild the index.
Each rule also indexes its alternatives by root signature. Specific and generic
routes merge in linear time, without allocating or testing unrelated alternatives.
Adding an unrelated rule does not add another full traversal or re-decode every
call and pipe. Normalization remains lazy: wildcards and skipped sequence items
do not inspect their subtrees.

The normalizer and structural verifier handle syntax variations. The compiler
does not expand every spelling, wrapper depth and pipe shape into generated
query alternatives.

## Reporting and existing features

`result = "discarded"` restricts a pattern to an expression whose value is
discarded in a recognized executable scope. For example:

```toml
pattern = "Sample.transform(...)"
report = "callee"
result = "discarded"
```

This uses the original expression, including an outer pipe, not its reported
callee token. Arguments, assignments, pipe inputs, and function returns consume
their values. Branch tails inherit their control expression's use. A `try`
cleanup discards its tail, but a `receive` timeout branch returns its tail.
Nested functions have independent returns.

Only recognized function/anonymous-function scopes and language control forms
are analyzed. Quoted syntax, module initializers, unknown macros and
comprehensions are unknown, not discarded. This includes ambiguous expression
sequences passed as arguments to unknown calls. This is syntax analysis, not proof of
purity, alias resolution or macro expansion. A discarded call can have intentional
effects; rules must not promise that removing it is safe. Result analysis shares
the pattern work budget, bounds ancestry depth, and runs once per matching
candidate even when several rules constrain its use. This option currently
requires `pattern`; it cannot silently apply to query, regex or path rules.

`report = "match"` (default) reports the whole expression. `report = "callee"`
reports the original function-name token, including its original quoting.
Operator-root patterns can use `report = "operator"` for the operator token.
Reported byte ranges always refer to the original source.

Pattern rules support existing severity, path filters, suppression aliases,
embedded tests and snapshots. `pattern` cannot be combined with another matcher,
or with low-level query stages, conditions or comparisons.

## Examples and tests

The [illustrative rules](../examples/patterns/.fino/rules/) show nil filtering,
identity maps and boolean cases. They are optional examples, not a bundled
application policy. Project-specific rules live in consumer repositories:

```sh
fino validate --root examples/patterns
fino test --root examples/patterns
```

These demonstrate intended behavior, not complete parity with legacy Credo
checks. Do not replace an existing policy without reviewing the differences.
Keep short behavioral examples in rules. Generate large clause-count, nesting
and resource-limit cases in Fino's Rust tests instead of committing giant code
strings to application rules.

## Schemas

Fino owns the rule and config schemas under [`schema/`](../schema/). The CLI
publishes the same schemas with `fino schema rule` and `fino schema config`.
Integration tests compare CLI output to the checked-in schemas.

Consumers should reference a published version, for example:

```toml
#:schema https://raw.githubusercontent.com/pdq/fino/main/schema/rule.schema.json
```

Use a release tag instead of `main` for a version-pinned reference. Application
repositories do not need to vendor copies. The installed CLI is authoritative
when testing an unreleased feature.
