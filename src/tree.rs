//! Human- and AI-readable syntax tree, for writing queries.
use std::fmt::Write;
use tree_sitter::Parser;

/// Render the syntax tree with field names, positions and leaf text.
pub fn render(source: &str) -> crate::Result<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&crate::engine::language())
        .map_err(|e| e.to_string())?;
    let tree = crate::scan::parse(&mut parser, source)?;
    let mut out = String::new();
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        let node = cursor.node();
        let start = node.start_position();
        let field = cursor
            .field_name()
            .map(|f| format!("{f}: "))
            .unwrap_or_default();
        let label = if node.is_named() {
            node.kind().to_string()
        } else {
            format!("{:?}", node.kind())
        };
        let _ = write!(
            out,
            "{}{field}{label} [{}:{}]",
            "  ".repeat(depth),
            start.row + 1,
            start.column + 1
        );
        if node.is_named() && node.child_count() == 0 {
            let text: String = source[node.byte_range()].chars().take(60).collect();
            let _ = write!(out, " {text:?}");
        }
        if node.is_error() || node.is_missing() {
            out.push_str("  <-- syntax error");
        }
        out.push('\n');
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(out);
            }
            depth -= 1;
        }
    }
}
