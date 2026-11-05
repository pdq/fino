# Local runtime patch

Source: the official `tree-sitter` **0.25.10** crates.io package
(<https://github.com/tree-sitter/tree-sitter/tree/v0.25.10/lib>), MIT-licensed; `LICENSE`
is included. Only `src/stack.c` and `src/stack.h` differ. Selected via `[patch.crates-io]`.

## Why

Upstream keeps at most eight predecessor links per parse-stack node and silently drops
the rest. Some valid, highly ambiguous Elixir syntax needs more. A reduced synthetic
case with nested anonymous functions is included in `src/fixtures/nested_case.ex`.

## Change

- The link limit is a per-thread value, default **8** (upstream). Link storage keeps
  eight links inline and spills extra links to the heap, freed before node reuse.
- `ts_beam_set_stack_link_limit(n)` changes the limit for the calling thread.
- Upstream's 64-path traversal limit is unchanged.

Fino parses every file with upstream limits. Only when that yields a syntax error
does it retry once with a limit of **64**. Each attempt has a 10 s budget and fails
closed. A first attempt that removed the limits entirely caused exponential blow-up on
a dependency file (`toml/decoder.ex`), so limits stay finite.

## Evidence

- `tests/runtime_stack.c` (ASan + UBSan): default limit keeps 8 links, raised limit
  keeps 129, pooled reuse is safe, traversal over spilled links is safe.
- 292/292 official grammar corpus cases match.
- The synthetic nested-case regression parses and preserves query findings.
- Grammar failures are reported as errors rather than guessed through.
