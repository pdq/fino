//! Blank or remove syntax content (strings, comments, ...) using the parse tree.
use crate::model::MaskNode;
use std::collections::BTreeMap;
use std::ops::Range;
use tree_sitter::{Node, Tree};

/// Byte ranges of the preset mask nodes, collected once per file.
#[derive(Default)]
pub(crate) struct Regions {
    ranges: BTreeMap<MaskNode, Vec<Range<usize>>>,
}

/// Content between `quoted_start` and `quoted_end`, or the whole node otherwise.
/// Heredoc content starts on the line after the opening delimiter: anything left on
/// the opening line (such as trailing whitespace) is not part of the value.
pub(crate) fn content_range(node: Node, text: &[u8]) -> Range<usize> {
    match (
        node.child_by_field_name("quoted_start"),
        node.child_by_field_name("quoted_end"),
    ) {
        (Some(open), Some(close)) if open.end_byte() <= close.start_byte() => {
            let (start, end) = (open.end_byte(), close.start_byte());
            let heredoc = matches!(&text[open.byte_range()], b"\"\"\"" | b"'''");
            match text[start..end].iter().position(|&b| b == b'\n') {
                Some(newline) if heredoc => start + newline..end,
                _ => start..end,
            }
        }
        _ => node.byte_range(),
    }
}

impl Regions {
    pub fn collect(tree: &Tree, text: &[u8]) -> Self {
        let mut regions = Self::default();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let kind = match node.kind() {
                "string" => Some(MaskNode::String),
                "charlist" => Some(MaskNode::Charlist),
                "sigil" => Some(MaskNode::Sigil),
                "quoted_atom" | "quoted_keyword" => Some(MaskNode::QuotedAtom),
                "comment" => Some(MaskNode::Comment),
                _ => None,
            };
            if let Some(kind) = kind {
                regions
                    .ranges
                    .entry(kind)
                    .or_default()
                    .push(content_range(node, text));
            }
            let mut walk = node.walk();
            stack.extend(node.children(&mut walk));
        }
        regions
    }

    pub fn get(&self, node: MaskNode) -> &[Range<usize>] {
        self.ranges.get(&node).map_or(&[], Vec::as_slice)
    }
}

pub(crate) struct Layer {
    pub ranges: Vec<Range<usize>>,
    pub with: Option<u8>,
    pub fill: bool,
}

/// Apply mask layers in order; a later layer wins where layers overlap.
/// Newlines are always kept, so line numbers never change.
pub(crate) fn apply(text: &str, layers: &[Layer]) -> String {
    let bytes = text.as_bytes();
    let mut owner = vec![0u16; bytes.len()];
    for (index, layer) in layers.iter().enumerate() {
        for range in &layer.ranges {
            owner[range.clone()].fill(index as u16 + 1);
        }
    }
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        let Some(layer) = owner[i].checked_sub(1).map(|o| &layers[o as usize]) else {
            out.push(b);
            continue;
        };
        if b == b'\n' {
            if layer.fill && i > 0 && bytes[i - 1] == b'\n' && owner[i - 1] != 0 {
                out.extend(layer.with);
            }
            out.push(b'\n');
        } else if let Some(w) = layer.with {
            out.push(w);
        }
    }
    // Ranges come from node boundaries and replacements are ASCII.
    String::from_utf8(out).expect("masking keeps UTF-8 valid")
}
