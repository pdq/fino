# Scoped matching

Tree-sitter queries match concrete syntax. A post-match predicate cannot recover
a node that the original structural query never captured. Stages address this
by capturing an expression slot first, then matching its selected root.

## Root stages

```toml
id = "marked-argument"
message = "Use a different argument."
query = '''
((call target: (identifier) @_target
       (arguments (_) @_expression) @_args) @finding
 (#eq? @_target "emit") (#count-eq? @_args 1))
'''

[[stages]]
from = "_expression"
relation = "root"
unwrap_singleton = ["block"]
ignore_children = ["comment"]
query = '((identifier) @_leaf (#eq? @_leaf "marker"))'
export = ["_leaf"]
```

The stage repeatedly unwraps a listed node only when exactly one named child
remains after ignoring the specified child types. Empty and multiple-child
containers remain intact. This handles arbitrary wrapper depth without writing
one query alternative per depth. It is not general AST normalization: select
wrapper kinds deliberately.

The stage query must match the selected root, not an arbitrary descendant.
Captures inside that rooted pattern may refer to descendants or anonymous tokens.
Exports become available to later stages, comparisons and conditions. Unknown
bindings, forward references and name collisions fail validation.

An outer query may omit `@finding` if a stage exports `finding`. Its original
source span then determines the diagnostic. A stage cannot overwrite an existing
outer finding. Failed stages produce no finding; ambiguous multiple stage matches
are errors, not an arbitrary first result.

## Scoped conditions

```toml
[[conditions]]
capture = "_leaf"
relation = "root"
query = '((identifier) @value (#eq? @value "marker"))'
expect = "exists"
```

Conditions are ANDed after stages and comparisons. `relation` defaults to
`descendant-or-self`; `root` restricts matching to the captured node. `expect`
can be `exists` or `absent`. Condition captures stay local. The referenced scope
must contain exactly one binding; missing or repeated bindings reject the outer
match rather than making an absence test vacuously succeed.

## Cross-stage comparisons

```toml
[[comparisons]]
left = "_left"
right = "_right"
operator = "text-eq"
```

`text-not-eq` is also supported. These compare captured source text, not decoded
literal values or ASTs with metadata removed. Ordinary query `#eq?` predicates
continue to operate within their own query.

Use `nfc-eq` or `nfc-not-eq` to compare captured text after Unicode canonical
normalization (NFC), for example composed and decomposed spellings of an
identifier. This is opt-in: ordinary `text-eq` stays byte-exact. NFC comparison
does not fold case, apply compatibility normalization, decode literal escapes,
or compare ASTs. Normalization charges both captures' byte lengths against the
per-file predicate-work budget and fails closed on exhaustion.

Optional `left_prefix` and `right_prefix` strings prepend literal text to the
respective capture before comparison. This can relate names that use different
surface prefixes without enumerating the possible names. Prefixes work with all
four operators and participate in NFC normalization as part of the complete
text, including composition across the prefix boundary. Prefixed comparisons
charge both prefixes and captures to the predicate-work budget; they stream the
comparison rather than allocating concatenated strings.

Queries compile once. Wrapper resolution and scoped results are cached per file.
Query overflow, work-budget exhaustion and ambiguous stages fail the file; they
never establish a successful negative condition. Rules without these features
retain the legacy matcher fast path.

Name the parent kind in auxiliary queries. Wildcard parents with children, such
as `(_ (identifier) @item)`, are explicitly unsupported because Tree-sitter can
optimize away their root. Leaf wildcards remain supported.

Auxiliary matching also has per-file candidate and predicate-work limits, cached
child counts, native progress cancellation, a two-second per-query deadline and
a five-second cumulative execution deadline. Exhaustion is an error, not a clean
match or a successful absence test.
