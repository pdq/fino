//! Conservative syntactic result-position analysis. This does not expand macros.
use crate::Result;
use crate::pattern::Work;
use tree_sitter::Node;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Usage {
    Discarded,
    Consumed,
    Unknown,
}

const MAX_DEPTH: usize = 256;

fn name<'a>(
    call: Node,
    source: &'a [u8],
    work: &mut Work,
) -> Result<Option<std::borrow::Cow<'a, str>>> {
    let Some(target) = call.child_by_field_name("target") else {
        return Ok(None);
    };
    // Only unqualified, static names are recognized as language forms.
    if target.kind() != "identifier" {
        return Ok(None);
    }
    work.charge(target.byte_range().len())?;
    Ok(crate::literal::value(target, source, false))
}

fn key<'a>(
    pair: Node,
    source: &'a [u8],
    work: &mut Work,
) -> Result<Option<std::borrow::Cow<'a, str>>> {
    let Some(node) = pair.child_by_field_name("key") else {
        return Ok(None);
    };
    work.charge(node.byte_range().len())?;
    Ok(crate::literal::value(node, source, true))
}

fn definition(name: &str) -> bool {
    matches!(name, "def" | "defp" | "defmacro" | "defmacrop")
}

fn control(name: &str) -> bool {
    matches!(
        name,
        "if" | "unless" | "case" | "cond" | "with" | "receive" | "try"
    )
}

fn alternative(node: Node) -> bool {
    matches!(
        node.kind(),
        "else_block" | "rescue_block" | "catch_block" | "after_block"
    )
}

// A clause is an alternative, not a later sequential expression.
fn later_expression(node: Node, work: &mut Work) -> Result<bool> {
    let mut sibling = node.next_named_sibling();
    while let Some(next) = sibling {
        work.charge(1)?;
        if next.kind() != "comment" && !alternative(next) && next.kind() != "stab_clause" {
            return Ok(true);
        }
        sibling = next.next_named_sibling();
    }
    Ok(false)
}

/// Classify an original expression node, not the token chosen for reporting.
///
/// Unknown syntax and non-executable scopes fail closed. The complete ancestry
/// is checked even after the result is known, notably to exclude quoted code.
pub(crate) fn classify(node: Node, source: &[u8], work: &mut Work) -> Result<Usage> {
    let mut current = node;
    let mut usage = None;
    let mut eligible = false;
    // Carries a block/keyword role through syntactic wrappers to its owning call.
    let mut role: Option<&str> = None;
    let mut depth = 0;
    while let Some(parent) = current.parent() {
        depth += 1;
        work.charge(1)?;
        if depth > MAX_DEPTH {
            return Err("result position nesting limit exceeded".into());
        }
        if parent.is_error() || parent.is_missing() {
            return Ok(Usage::Unknown);
        }
        match parent.kind() {
            "body" | "block" => {
                if usage.is_none()
                    && current.kind() != "stab_clause"
                    && later_expression(current, work)?
                {
                    usage = Some(Usage::Discarded);
                }
            }
            "do_block" | "else_block" | "rescue_block" | "catch_block" | "after_block" => {
                if usage.is_none()
                    && current.kind() != "stab_clause"
                    && later_expression(current, work)?
                {
                    usage = Some(Usage::Discarded);
                }
                if parent.kind() == "after_block" && usage.is_none() {
                    // `receive ... after timeout -> value` returns value.
                    // Only a try/implicit-try cleanup block discards its tail.
                    let owner = parent.parent().and_then(|block| block.parent());
                    if let Some(owner) = owner {
                        let n = name(owner, source, work)?;
                        if n.as_deref() == Some("try") || n.as_deref().is_some_and(definition) {
                            usage = Some(Usage::Discarded);
                        }
                    }
                }
                role = Some(match parent.kind() {
                    "do_block" => "do",
                    "else_block" => "else",
                    "rescue_block" => "rescue",
                    "catch_block" => "catch",
                    _ => "after",
                });
            }
            "stab_clause" => {
                if parent.child_by_field_name("right") != Some(current) {
                    // Clause patterns and guards are not executable bodies.
                    return Ok(Usage::Unknown);
                }
            }
            "anonymous_function" => {
                eligible = true;
                usage.get_or_insert(Usage::Consumed);
                role = None;
            }
            "pair" => {
                if parent.child_by_field_name("value") != Some(current) {
                    return Ok(Usage::Unknown);
                }
                let k = key(parent, source, work)?;
                role = match k.as_deref() {
                    Some("do") => Some("do"),
                    Some("else") => Some("else"),
                    Some("rescue") => Some("rescue"),
                    Some("catch") => Some("catch"),
                    Some("after") => Some("after"),
                    _ => {
                        usage.get_or_insert(Usage::Consumed);
                        None
                    }
                };
            }
            "keywords" => {}
            "arguments" => {
                if role.is_none() {
                    // A sequence passed as an argument may be syntax consumed
                    // by an unknown macro, not an evaluated argument block.
                    // A nested fn has its own established return scope.
                    if usage == Some(Usage::Discarded) && !eligible {
                        let owner = parent.parent();
                        let n = match owner {
                            Some(owner) => name(owner, source, work)?,
                            None => None,
                        };
                        if !n.as_deref().is_some_and(control) {
                            return Ok(Usage::Unknown);
                        }
                    }
                    usage.get_or_insert(Usage::Consumed);
                }
            }
            "call" => {
                let n = name(parent, source, work)?;
                if n.as_deref() == Some("quote") {
                    return Ok(Usage::Unknown);
                }
                if n.as_deref().is_some_and(definition) {
                    if role != Some("do") {
                        return Ok(Usage::Unknown);
                    }
                    eligible = true;
                    usage.get_or_insert(Usage::Consumed);
                } else if let Some(block_role) = role {
                    let Some(n) = n.as_deref() else {
                        return Ok(Usage::Unknown);
                    };
                    // A module body is not an executable return scope, but it
                    // commonly encloses the definition already established.
                    if n == "defmodule" && eligible && block_role == "do" {
                        role = None;
                        current = parent;
                        continue;
                    }
                    if !control(n) {
                        return Ok(Usage::Unknown);
                    }
                    if block_role == "after" {
                        if n != "try" {
                            return Ok(Usage::Unknown);
                        }
                        usage.get_or_insert(Usage::Discarded);
                    }
                } else {
                    usage.get_or_insert(Usage::Consumed);
                }
                role = None;
            }
            "binary_operator" | "unary_operator" => {
                let Some(op) = parent.child_by_field_name("operator") else {
                    return Ok(Usage::Unknown);
                };
                work.charge(op.byte_range().len())?;
                let operator = source.get(op.byte_range()).unwrap_or_default();
                if operator == b"@" {
                    return Ok(Usage::Unknown);
                }
                // The right hand target is the pipe's result; its input is used.
                if operator != b"|>" || parent.child_by_field_name("right") != Some(current) {
                    usage.get_or_insert(Usage::Consumed);
                }
                role = None;
            }
            "list" | "tuple" | "map" | "interpolation" => {
                usage.get_or_insert(Usage::Consumed);
                role = None;
            }
            "source" => {}
            // Crossing an unrecognized syntax container cannot prove result use.
            _ => return Ok(Usage::Unknown),
        }
        current = parent;
    }
    Ok(if eligible {
        usage.unwrap_or(Usage::Unknown)
    } else {
        Usage::Unknown
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    // Select the expression by its unique source text, independent of callee names.
    fn usage(code: &str, needle: &str) -> Usage {
        let mut parser = Parser::new();
        parser.set_language(&crate::engine::language()).unwrap();
        let tree = crate::scan::parse(&mut parser, code).unwrap();
        assert!(!tree.root_node().has_error(), "{code}");
        let start = code.find(needle).unwrap();
        let end = start + needle.len();
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(start, end)
            .unwrap();
        while node.start_byte() != start || node.end_byte() != end {
            node = node.parent().expect("expression span");
        }
        classify(node, code.as_bytes(), &mut Work::default()).unwrap()
    }

    #[test]
    fn sequences_and_consumers() {
        use Usage::*;
        for (code, expected) in [
            ("def f(x) do\n A.b(x)\n :ok\nend", Discarded),
            ("def f(x), do: A.b(x)", Consumed),
            (
                "defmodule M do\n def f(x) do\n A.b(x)\n :ok\n end\nend",
                Discarded,
            ),
            ("def f(x) do\n sink(A.b(x))\n :ok\nend", Consumed),
            ("def f(x) do\n _ = A.b(x)\n :ok\nend", Consumed),
            ("def f(x) do\n A.b(x) <> x\n :ok\nend", Consumed),
            ("def f(x) do\n [A.b(x)]\n :ok\nend", Consumed),
            ("def f(x) do\n custom((A.b(x); :ok))\n :ok\nend", Unknown),
            (
                "def f(x) do\n sink(fn -> A.b(x); :ok end)\n :ok\nend",
                Discarded,
            ),
            ("A.b(x); :ok", Unknown),
        ] {
            assert_eq!(usage(code, "A.b(x)"), expected, "{code}");
        }
    }

    #[test]
    fn branches_scopes_and_quotes() {
        use Usage::*;
        for (code, expected) in [
            (
                "def f(x) do\n if x do\n A.b(x)\n else\n :ok\n end\n :ok\nend",
                Discarded,
            ),
            ("def f(x), do: if(x, do: A.b(x), else: :ok)", Consumed),
            (
                "def f(x) do\n case x do\n :a -> A.b(x)\n _ -> :ok\n end\n :ok\nend",
                Discarded,
            ),
            ("def f(x) do\n fn y -> A.b(x) end\n :ok\nend", Consumed),
            (
                "def f(x) do\n try do\n :ok\n after\n A.b(x)\n end\nend",
                Discarded,
            ),
            (
                "def f(x) do\n receive do\n :ok -> :ok\n after\n 10 -> A.b(x)\n end\nend",
                Consumed,
            ),
            (
                "def f(x) do\n receive do\n :ok -> :ok\n after\n 10 -> A.b(x)\n end\n :ok\nend",
                Discarded,
            ),
            ("def f(x) do\n quote do\n A.b(x); :ok\n end\nend", Unknown),
            ("quote do\n def f(x) do\n A.b(x); :ok\n end\nend", Unknown),
            ("def f(x) do\n custom do\n A.b(x); :ok\n end\nend", Unknown),
            (
                "def f(x) do\n for y <- x do\n A.b(x); y\n end\nend",
                Unknown,
            ),
        ] {
            assert_eq!(usage(code, "A.b(x)"), expected, "{code}");
        }
    }

    #[test]
    fn pipe_positions() {
        use Usage::*;
        assert_eq!(
            usage("def f(x) do\n x |> A.b()\n :ok\nend", "x |> A.b()"),
            Discarded
        );
        assert_eq!(
            usage(
                "def f(x) do\n x |> A.b() |> sink()\n :ok\nend",
                "x |> A.b()"
            ),
            Consumed
        );
    }
}
