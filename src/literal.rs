//! Static Elixir names and atoms. Decode syntax; never evaluate source.
use std::borrow::Cow;
use tree_sitter::Node;
use unicode_normalization::UnicodeNormalization;

pub(crate) const PREDICATES: [&str; 4] = ["name-eq?", "name-any-of?", "atom-eq?", "atom-any-of?"];

/// Names are identifiers or quoted remote-call names. Atoms are a distinct
/// category: a string containing "false" must not be treated as the atom false.
pub(crate) fn value<'a>(mut node: Node, source: &'a [u8], atom: bool) -> Option<Cow<'a, str>> {
    while node.kind() == "block" {
        let mut cursor = node.walk();
        let mut children = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() != "comment");
        let first = children.next()?;
        if children.next().is_some() {
            return None;
        }
        node = first;
    }
    let text = std::str::from_utf8(source.get(node.byte_range())?).ok()?;
    match (atom, node.kind()) {
        (false, "identifier") if !text.is_ascii() => Some(Cow::Owned(text.nfc().collect())),
        (false, "identifier") | (true, "boolean" | "nil") => Some(Cow::Borrowed(text)),
        (true, "atom") => text.strip_prefix(':').map(Cow::Borrowed),
        (true, "quoted_atom") => quoted(node, text.strip_prefix(':')?),
        (true, "keyword") => text.trim_end().strip_suffix(':').map(Cow::Borrowed),
        (true, "quoted_keyword") => quoted(node, text.trim_end().strip_suffix(':')?),
        (false, "string" | "charlist") => quoted(node, text),
        _ => None,
    }
}

fn quoted<'a>(node: Node, text: &'a str) -> Option<Cow<'a, str>> {
    // Only ordinary, static quoted names are supported. Heredocs have indentation
    // semantics and are not remote names. Interpolation is never executed.
    let quote = *text.as_bytes().first()?;
    if !matches!(quote, b'"' | b'\'')
        || text.as_bytes().last() != Some(&quote)
        || text.as_bytes().get(1..3) == Some(&[quote, quote])
    {
        return None;
    }
    let mut cursor = node.walk();
    if node
        .named_children(&mut cursor)
        .any(|child| child.kind() == "interpolation")
    {
        return None;
    }
    decode(text.get(1..text.len().checked_sub(1)?)?)
}

fn hex(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<u32> {
    (0..digits).try_fold(0, |n, _| Some(n * 16 + chars.next()?.to_digit(16)?))
}

fn decode(text: &str) -> Option<Cow<'_, str>> {
    if !text.contains('\\') {
        return Some(Cow::Borrowed(text));
    }
    // Hex escapes represent bytes, not Unicode codepoints. Collect bytes and
    // validate UTF-8 after decoding so "\xC3\xA9" correctly becomes "é".
    let mut result = Vec::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        let value = if c != '\\' {
            c
        } else {
            match chars.next()? {
                '\n' => continue,
                '\r' => {
                    if chars.next()? != '\n' {
                        return None;
                    }
                    continue;
                }
                'a' => '\x07',
                'b' => '\x08',
                'd' => '\x7f',
                'e' => '\x1b',
                'f' => '\x0c',
                'n' => '\n',
                'r' => '\r',
                's' => ' ',
                't' => '\t',
                'v' => '\x0b',
                '0' => '\0',
                'x' => {
                    result.push(hex(&mut chars, 2)? as u8);
                    continue;
                }
                'u' => {
                    let codepoint = if chars.as_str().starts_with('{') {
                        chars.next();
                        let mut n = 0u32;
                        let mut digits = 0;
                        loop {
                            let c = chars.next()?;
                            if c == '}' {
                                if digits == 0 {
                                    return None;
                                }
                                break;
                            }
                            n = n.checked_mul(16)?.checked_add(c.to_digit(16)?)?;
                            digits += 1;
                        }
                        n
                    } else {
                        hex(&mut chars, 4)?
                    };
                    char::from_u32(codepoint)?
                }
                // Elixir drops the slash on unrecognized escapes, e.g. "\l".
                other => other,
            }
        };
        result.extend_from_slice(value.encode_utf8(&mut [0; 4]).as_bytes());
    }
    String::from_utf8(result).ok().map(Cow::Owned)
}

fn matches(
    operator: &str,
    node: Node,
    source: &[u8],
    expected: &[tree_sitter::QueryPredicateArg],
) -> bool {
    let Some(value) = value(node, source, operator.starts_with("atom-")) else {
        return false;
    };
    expected.iter().any(|arg| {
        matches!(arg, tree_sitter::QueryPredicateArg::String(text) if text.as_ref() == value.as_ref())
    })
}

/// Shared predicate semantics. Scoped queries supply bounded accounting; the
/// ordinary scanner supplies an infallible callback.
pub(crate) fn predicate(
    predicate: &tree_sitter::QueryPredicate,
    found: &tree_sitter::QueryMatch,
    source: &[u8],
    mut account: impl FnMut(usize) -> crate::Result<()>,
) -> crate::Result<bool> {
    let Some(tree_sitter::QueryPredicateArg::Capture(index)) = predicate.args.first() else {
        return Ok(false);
    };
    account(found.captures.len())?;
    for arg in &predicate.args[1..] {
        if let tree_sitter::QueryPredicateArg::String(text) = arg {
            account(text.len())?;
        }
    }
    let mut present = false;
    for capture in found.captures.iter().filter(|c| c.index == *index) {
        present = true;
        account(capture.node.byte_range().len())?;
        if !matches(
            &predicate.operator,
            capture.node,
            source,
            &predicate.args[1..],
        ) {
            return Ok(false);
        }
    }
    Ok(present)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_elixir_escapes_without_evaluation() {
        for (source, expected) in [
            (r"f\x69lter", "filter"),
            (r"f\u0069lter", "filter"),
            (r"f\u{000069}lter", "filter"),
            (r"fi\lter", "filter"),
            ("fil\\\nter", "filter"),
            ("fil\\\r\nter", "filter"),
            (r"\xC3\xA9", "é"),
            (r"\u{1F600}", "😀"),
            (
                r"\0\a\b\d\e\f\n\r\s\t\v",
                "\0\x07\x08\x7f\x1b\x0c\n\r \t\x0b",
            ),
            (r#"\"\\\#"#, "\"\\#"),
        ] {
            assert_eq!(decode(source).as_deref(), Some(expected), "{source:?}");
        }
        for source in [
            r"\x",
            r"\xGG",
            r"\u{}",
            r"\uD800",
            r"\u{110000}",
            "\\",
            r"\xFF",
        ] {
            assert_eq!(decode(source), None, "{source:?}");
        }
    }
}
