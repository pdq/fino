# Local character-token fix

Upstream: `tree-sitter-elixir` 0.3.5, Apache-2.0. Original file digests are in
`UPSTREAM-SHA256.json`. The license and Rust build/bindings are preserved.

The character token must allow actual NUL and LF codepoints, including after a
backslash. The generated lexer previously excluded both. `lookahead == 0` is
not EOF: Tree-sitter explicitly provides `lexer->eof` to distinguish a literal
NUL from the end of input.

The grammar and grammar JSON use an all-codepoint class instead of dot. The two
generated character continuations (states 206 and 406) test `!eof`. No source
bytes are replaced, removed or normalized. Other lexer states are unchanged.
State 406 retains its existing accepting state: a backslash itself is a valid
character at EOF. A lone question mark still fails parsing.

Regression tests cover raw and escaped control characters, actual NUL/LF, EOF,
invalid NUL outside character tokens, and exact source-byte ranges. Reapply and
review these two continuations if regenerating the parser; do not copy a generic
`lookahead != 0` check back over them. No upstream submission has been made.
