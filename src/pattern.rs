//! Elixir-shaped patterns: a small candidate query plus lazy structural matching.
//! Only syntax demanded by a pattern is normalized. No Elixir code is evaluated.
use crate::Result;
use crate::model::{PatternCaptureKind, PatternReport, Patterns};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use tree_sitter::{Node, Parser};

const MAX_DEPTH: usize = 128;
const MAX_WORK: usize = 2_000_000;
pub(crate) const CANDIDATE_QUERY: &str =
    "(call) @finding\n(binary_operator operator: \"|>\") @finding";
const CONTEXT_CONSTANTS: [&str; 5] = [
    "__MODULE__",
    "__ENV__",
    "__DIR__",
    "__CALLER__",
    "__STACKTRACE__",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    Capture,
    Rest,
    Variable,
    Context,
    Alias,
    Atom,
    String,
    Charlist,
    Integer,
    LocalCall,
    RemoteCall,
    Unary,
    Binary,
    Function,
    Clause,
    Parameters,
    Tuple,
    List,
    Map,
    Block,
    Unsupported,
}

#[derive(Debug)]
struct Term {
    kind: Kind,
    value: String,
    children: Vec<Term>,
}

impl Term {
    fn same(&self, other: &Self, work: &mut Work) -> Result<bool> {
        work.charge(1 + self.value.len() + other.value.len())?;
        if self.kind != other.kind
            || self.value != other.value
            || self.children.len() != other.children.len()
        {
            return Ok(false);
        }
        for (a, b) in self.children.iter().zip(&other.children) {
            if !a.same(b, work)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn has_capture(&self, name: &str) -> bool {
        (self.kind == Kind::Capture && self.value == name)
            || self.children.iter().any(|child| child.has_capture(name))
    }
}

pub(crate) struct Pattern {
    alternatives: Vec<Term>,
    signatures: Vec<Root>,
    alternative_routes: crate::pattern_routes::Index,
    report: PatternReport,
    constraints: BTreeMap<String, PatternCaptureKind>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Root {
    kind: Kind,
    name: String,
    module: Option<(Kind, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Signature<'a> {
    pub kind: Kind,
    pub name: &'a str,
    pub module: Option<(Kind, &'a str)>,
}

pub(crate) struct Candidate<'text, 'tree> {
    pub node: Node<'tree>,
    kind: Kind,
    name: Cow<'text, str>,
    module: Option<(Kind, Cow<'text, str>)>,
    callee: Range<usize>,
}

impl Candidate<'_, '_> {
    pub(crate) fn signature(&self) -> Signature<'_> {
        Signature {
            kind: self.kind,
            name: &self.name,
            module: self
                .module
                .as_ref()
                .map(|(kind, name)| (*kind, name.as_ref())),
        }
    }
}

impl Term {
    fn signature(&self) -> Signature<'_> {
        let module = (self.kind == Kind::RemoteCall)
            .then(|| self.children.first())
            .flatten()
            .filter(|module| matches!(module.kind, Kind::Alias | Kind::Atom | Kind::Context))
            .map(|module| (module.kind, module.value.as_str()));
        Signature {
            kind: self.kind,
            name: &self.value,
            module,
        }
    }
}

#[derive(Default)]
pub(crate) struct Work {
    spent: usize,
}

impl Work {
    pub(crate) fn charge(&mut self, amount: usize) -> Result<()> {
        self.spent = self
            .spent
            .checked_add(amount)
            .ok_or("pattern work limit exceeded")?;
        if self.spent > MAX_WORK {
            return Err("pattern work limit exceeded".into());
        }
        Ok(())
    }
}

/// Synthetic keyword nodes make `do ... end`, `do: ...` and `[{:do, ...}]`
/// share a structural view without rewriting the source or manufacturing trees.
#[derive(Clone, Copy)]
enum Input<'tree> {
    Node(Node<'tree>),
    BlockPair {
        key: &'static str,
        node: Node<'tree>,
        outer: bool,
    },
    BlockKey(&'static str),
    BlockBody {
        node: Node<'tree>,
        outer: bool,
    },
}

fn block_key(kind: &str) -> Option<&'static str> {
    match kind {
        "else_block" => Some("else"),
        "rescue_block" => Some("rescue"),
        "catch_block" => Some("catch"),
        "after_block" => Some("after"),
        _ => None,
    }
}

struct View<'text, 'tree> {
    kind: Kind,
    value: Cow<'text, str>,
    children: Vec<Input<'tree>>,
    callee: Option<Range<usize>>,
}

impl View<'_, '_> {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            value: Cow::Borrowed(""),
            children: vec![],
            callee: None,
        }
    }
}

struct Normalizer<'text, 'work> {
    source: &'text [u8],
    work: &'work mut Work,
}

impl<'text> Normalizer<'text, '_> {
    fn text(&mut self, node: Node) -> Result<&'text str> {
        self.work.charge(node.byte_range().len())?;
        std::str::from_utf8(&self.source[node.byte_range()]).map_err(|e| e.to_string())
    }

    fn named<'tree>(&mut self, node: Node<'tree>) -> Result<Vec<Node<'tree>>> {
        let mut cursor = node.walk();
        let mut children = Vec::new();
        for child in node.named_children(&mut cursor) {
            self.work.charge(1)?;
            if child.kind() != "comment" {
                children.push(child);
            }
        }
        Ok(children)
    }

    fn sole<'tree>(&mut self, node: Node<'tree>) -> Result<Option<Node<'tree>>> {
        let mut cursor = node.walk();
        let mut first = None;
        for child in node.named_children(&mut cursor) {
            self.work.charge(1)?;
            if child.kind() != "comment" {
                if first.is_some() {
                    return Ok(None);
                }
                first = Some(child);
            }
        }
        Ok(first)
    }

    fn unwrap<'tree>(&mut self, mut node: Node<'tree>) -> Result<Node<'tree>> {
        while matches!(node.kind(), "source" | "body" | "block") {
            self.work.charge(1)?;
            let Some(child) = self.sole(node)? else { break };
            node = child;
        }
        Ok(node)
    }

    fn decoded(&mut self, node: Node, atom: bool) -> Result<Option<Cow<'text, str>>> {
        // Charge input before decoding or allocating. Short output can still
        // require substantial work (e.g. thousands of escaped newlines).
        self.work.charge(node.byte_range().len())?;
        Ok(crate::literal::value(node, self.source, atom))
    }

    fn signature<'tree>(
        &mut self,
        node: Node<'tree>,
    ) -> Result<Option<(View<'text, 'tree>, Option<Node<'tree>>)>> {
        if node.kind() == "identifier" {
            let mut view = View::new(Kind::LocalCall);
            view.value = self.decoded(node, false)?.ok_or("invalid call name")?;
            view.callee = Some(node.byte_range());
            return Ok(Some((view, None)));
        }
        if node.kind() != "call" {
            return Ok(None);
        }
        let target = node
            .child_by_field_name("target")
            .ok_or("call has no target")?;
        match target.kind() {
            "identifier" => {
                let mut view = View::new(Kind::LocalCall);
                view.value = self.decoded(target, false)?.ok_or("invalid call name")?;
                view.callee = Some(target.byte_range());
                Ok(Some((view, None)))
            }
            "dot" => {
                let Some(name) = target.child_by_field_name("right") else {
                    return Ok(None);
                };
                let Some(value) = self.decoded(name, false)? else {
                    return Ok(None);
                };
                let module = target
                    .child_by_field_name("left")
                    .ok_or("call has no module")?;
                let mut view = View::new(Kind::RemoteCall);
                view.value = value;
                view.callee = Some(name.byte_range());
                Ok(Some((view, Some(module))))
            }
            _ => Ok(None),
        }
    }

    fn operator(&mut self, node: Node) -> Result<&'text str> {
        self.text(
            node.child_by_field_name("operator")
                .ok_or("missing operator")?,
        )
    }

    fn call<'tree>(
        &mut self,
        node: Node<'tree>,
        input: Option<Node<'tree>>,
    ) -> Result<View<'text, 'tree>> {
        let Some((mut view, module)) = self.signature(node)? else {
            return Ok(View::new(Kind::Unsupported));
        };
        if let Some(module) = module {
            view.children.push(Input::Node(module));
        }
        if let Some(input) = input {
            view.children.push(Input::Node(input));
        }
        for child in self.named(node)? {
            match child.kind() {
                "arguments" => view
                    .children
                    .extend(self.named(child)?.into_iter().map(Input::Node)),
                "do_block" => view.children.push(Input::Node(child)),
                _ => {}
            }
        }
        Ok(view)
    }

    fn view<'tree>(&mut self, input: Input<'tree>) -> Result<View<'text, 'tree>> {
        self.work.charge(1)?;
        let node = match input {
            Input::BlockKey(key) => {
                let mut view = View::new(Kind::Atom);
                view.value = Cow::Borrowed(key);
                return Ok(view);
            }
            Input::BlockPair { key, node, outer } => {
                let mut view = View::new(Kind::Tuple);
                view.children = vec![Input::BlockKey(key), Input::BlockBody { node, outer }];
                return Ok(view);
            }
            Input::BlockBody { node, outer } => {
                let children: Vec<_> = self
                    .named(node)?
                    .into_iter()
                    .filter(|child| !outer || block_key(child.kind()).is_none())
                    .collect();
                if let [child] = children.as_slice() {
                    return self.view(Input::Node(*child));
                }
                let mut view = View::new(Kind::Block);
                view.children = children.into_iter().map(Input::Node).collect();
                return Ok(view);
            }
            Input::Node(node) => self.unwrap(node)?,
        };
        let mut view = View::new(match node.kind() {
            "identifier" => {
                let text = self.text(node)?;
                if CONTEXT_CONSTANTS.contains(&text) {
                    Kind::Context
                } else {
                    Kind::Variable
                }
            }
            "alias" => Kind::Alias,
            "boolean" | "nil" | "atom" | "quoted_atom" | "keyword" | "quoted_keyword" => Kind::Atom,
            "string" => Kind::String,
            "charlist" => Kind::Charlist,
            "integer" => Kind::Integer,
            "anonymous_function" => Kind::Function,
            "stab_clause" => Kind::Clause,
            "arguments" => Kind::Parameters,
            "tuple" | "pair" => Kind::Tuple,
            "list" | "keywords" | "do_block" => Kind::List,
            "map" if self.named(node)?.is_empty() => Kind::Map,
            "source" | "body" | "block" => Kind::Block,
            "binary_operator" => Kind::Binary,
            "unary_operator" => Kind::Unary,
            "call" => return self.call(node, None),
            _ => Kind::Unsupported,
        });
        match view.kind {
            Kind::Variable | Kind::Context => {
                view.value = self.decoded(node, false)?.ok_or("invalid identifier")?;
            }
            Kind::Alias => {
                let text = self.text(node)?;
                view.value = if text.chars().any(|ch| ch.is_whitespace() || ch == '#') {
                    Cow::Owned(
                        text.lines()
                            .map(|line| line.split('#').next().unwrap_or(""))
                            .flat_map(str::chars)
                            .filter(|ch| !ch.is_whitespace())
                            .collect(),
                    )
                } else {
                    Cow::Borrowed(text)
                };
            }
            Kind::Atom | Kind::String | Kind::Charlist => {
                let Some(value) = self.decoded(node, view.kind == Kind::Atom)? else {
                    return Ok(View::new(Kind::Unsupported));
                };
                view.value = value;
            }
            Kind::Integer => {
                let raw = self.text(node)?.replace('_', "");
                let (digits, radix) = match raw.get(..2) {
                    Some("0x" | "0X") => (&raw[2..], 16),
                    Some("0o" | "0O") => (&raw[2..], 8),
                    Some("0b" | "0B") => (&raw[2..], 2),
                    _ => (raw.as_str(), 10),
                };
                let Ok(number) = u128::from_str_radix(digits, radix) else {
                    return Ok(View::new(Kind::Unsupported));
                };
                view.value = Cow::Owned(number.to_string());
            }
            Kind::Unary | Kind::Binary => {
                view.value = Cow::Borrowed(self.operator(node)?);
                if view.value == "|>" {
                    let left = node
                        .child_by_field_name("left")
                        .ok_or("missing pipe input")?;
                    let right = self.unwrap(
                        node.child_by_field_name("right")
                            .ok_or("missing pipe target")?,
                    )?;
                    return self.call(right, Some(left));
                }
                for field in ["left", "right", "operand"] {
                    if let Some(child) = node.child_by_field_name(field) {
                        view.children.push(Input::Node(child));
                    }
                }
            }
            Kind::List if node.kind() == "do_block" => {
                view.children.push(Input::BlockPair {
                    key: "do",
                    node,
                    outer: true,
                });
                for child in self.named(node)? {
                    if let Some(key) = block_key(child.kind()) {
                        view.children.push(Input::BlockPair {
                            key,
                            node: child,
                            outer: false,
                        });
                    }
                }
            }
            Kind::List if node.kind() == "list" => {
                for child in self.named(node)? {
                    if child.kind() == "keywords" {
                        view.children
                            .extend(self.named(child)?.into_iter().map(Input::Node));
                    } else {
                        view.children.push(Input::Node(child));
                    }
                }
            }
            Kind::Function
            | Kind::Clause
            | Kind::Parameters
            | Kind::Tuple
            | Kind::List
            | Kind::Block => {
                view.children = self.named(node)?.into_iter().map(Input::Node).collect();
            }
            Kind::Unsupported => view.value = Cow::Borrowed(node.kind()),
            _ => {}
        }
        Ok(view)
    }

    /// Materialize only the pattern itself, or a source capture whose repeated
    /// occurrence demands structural equality. Wildcards never enter this path.
    fn term(&mut self, input: Input, pattern: bool, depth: usize) -> Result<Term> {
        if depth > MAX_DEPTH {
            return Err("pattern nesting limit exceeded".into());
        }
        let view = self.view(input)?;
        if view.kind == Kind::Unsupported {
            return Err(format!(
                "unsupported {} syntax: {}",
                if pattern {
                    "pattern"
                } else {
                    "repeated capture equality"
                },
                view.value
            ));
        }
        if pattern
            && view.kind == Kind::LocalCall
            && matches!(
                view.value.as_ref(),
                "def" | "defp" | "defmacro" | "defmacrop"
            )
        {
            return Err("definition patterns are not supported by this prototype".into());
        }
        if pattern && view.kind == Kind::Unary && view.value == "@" {
            return Err(
                "attribute patterns need explicit name/value semantics and are not yet supported"
                    .into(),
            );
        }
        let kind = if pattern && view.kind == Kind::Variable && view.value == "..." {
            Kind::Rest
        } else if pattern && view.kind == Kind::Variable {
            Kind::Capture
        } else {
            view.kind
        };
        let children = view
            .children
            .into_iter()
            .map(|child| self.term(child, pattern, depth + 1))
            .collect::<Result<Vec<_>>>()?;
        let rest_count = children
            .iter()
            .filter(|child| child.kind == Kind::Rest)
            .count();
        if rest_count > 1 {
            return Err("at most one ellipsis is allowed per pattern sequence".into());
        }
        if rest_count > 0
            && !matches!(
                kind,
                Kind::LocalCall
                    | Kind::RemoteCall
                    | Kind::Parameters
                    | Kind::List
                    | Kind::Tuple
                    | Kind::Block
            )
        {
            return Err(
                "ellipsis is only supported in call arguments, parameters, lists, tuples or blocks"
                    .into(),
            );
        }
        Ok(Term {
            kind,
            value: view.value.into_owned(),
            children,
        })
    }

    fn matches<'pattern, 'tree>(
        &mut self,
        pattern: &'pattern Term,
        source: Input<'tree>,
        captures: &mut BTreeMap<&'pattern str, Input<'tree>>,
        constraints: &BTreeMap<String, PatternCaptureKind>,
        depth: usize,
    ) -> Result<bool> {
        self.work.charge(1)?;
        if depth > MAX_DEPTH {
            return Err("pattern nesting limit exceeded".into());
        }
        if pattern.kind == Kind::Capture {
            if pattern.value.starts_with('_') {
                return Ok(true);
            }
            if let Some(constraint) = constraints.get(&pattern.value) {
                let expected = match constraint {
                    PatternCaptureKind::Variable => Kind::Variable,
                    PatternCaptureKind::Atom => Kind::Atom,
                };
                if self.view(source)?.kind != expected {
                    return Ok(false);
                }
            }
            return match captures.get(pattern.value.as_str()) {
                Some(previous) => {
                    let left = self.term(*previous, false, 0)?;
                    let right = self.term(source, false, 0)?;
                    left.same(&right, self.work)
                }
                None => {
                    captures.insert(&pattern.value, source);
                    Ok(true)
                }
            };
        }
        let view = self.view(source)?;
        self.work.charge(pattern.value.len() + view.value.len())?;
        if pattern.kind != view.kind || pattern.value != view.value {
            return Ok(false);
        }
        self.work.charge(pattern.children.len())?;
        let rest = pattern
            .children
            .iter()
            .position(|child| child.kind == Kind::Rest);
        let prefix = rest.unwrap_or(pattern.children.len());
        let suffix = rest.map_or(0, |position| pattern.children.len() - position - 1);
        if view.children.len() < prefix + suffix
            || (rest.is_none() && view.children.len() != prefix)
        {
            return Ok(false);
        }
        // A single sequence hole has fixed prefix/suffix alignment. No search,
        // backtracking or materialization of skipped expressions is needed.
        for (child, source) in pattern.children[..prefix]
            .iter()
            .zip(&view.children[..prefix])
            .chain(
                pattern.children[pattern.children.len() - suffix..]
                    .iter()
                    .zip(&view.children[view.children.len() - suffix..]),
            )
        {
            if !self.matches(child, *source, captures, constraints, depth + 1)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl Pattern {
    pub(crate) fn compile(
        spec: &Patterns,
        report: PatternReport,
        constraints: &BTreeMap<String, PatternCaptureKind>,
    ) -> Result<Self> {
        if spec.alternatives().is_empty() {
            return Err("pattern needs at least one alternative".into());
        }
        let mut parser = Parser::new();
        parser
            .set_language(&crate::engine::language())
            .map_err(|e| e.to_string())?;
        let mut alternatives = Vec::new();
        let mut signatures = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut work = Work::default();
        for text in spec.alternatives() {
            work.charge(text.len())?;
            if !seen.insert(text.as_str()) {
                continue;
            }
            let tree = crate::scan::parse(&mut parser, text)?;
            if tree.root_node().has_error() {
                return Err("pattern contains an Elixir syntax error or missing delimiter".into());
            }
            let term = Normalizer {
                source: text.as_bytes(),
                work: &mut work,
            }
            .term(Input::Node(tree.root_node()), true, 0)?;
            for name in constraints.keys() {
                if name.starts_with('_') || !term.has_capture(name) {
                    return Err(format!(
                        "where: {name:?} must name a capture in every pattern alternative"
                    ));
                }
            }
            match term.kind {
                Kind::LocalCall | Kind::RemoteCall => {
                    if matches!(report, PatternReport::Operator) {
                        return Err("report = operator requires an operator-root pattern".into());
                    }
                }
                Kind::Unary | Kind::Binary => {
                    if matches!(report, PatternReport::Callee) {
                        return Err("report = callee requires a call-root pattern".into());
                    }
                }
                _ => return Err("patterns must have a call or operator at the root".into()),
            }
            let signature = term.signature();
            signatures.insert(Root {
                kind: signature.kind,
                name: signature.name.to_owned(),
                module: signature.module.map(|(kind, name)| (kind, name.to_owned())),
            });
            alternatives.push(term);
        }
        let mut alternative_routes = crate::pattern_routes::Index::default();
        for (index, term) in alternatives.iter().enumerate() {
            alternative_routes.add(term.signature(), index);
        }
        Ok(Self {
            alternatives,
            alternative_routes,
            signatures: signatures.into_iter().collect(),
            report,
            constraints: constraints.clone(),
        })
    }

    pub(crate) fn signatures(&self) -> impl Iterator<Item = Signature<'_>> {
        self.signatures.iter().map(|root| Signature {
            kind: root.kind,
            name: &root.name,
            module: root
                .module
                .as_ref()
                .map(|(kind, name)| (*kind, name.as_str())),
        })
    }

    pub(crate) fn candidate_query<'a>(patterns: impl Iterator<Item = &'a Self>) -> String {
        let kinds: BTreeSet<_> = patterns
            .flat_map(|pattern| pattern.signatures().map(|signature| signature.kind))
            .collect();
        let calls = kinds.contains(&Kind::LocalCall) || kinds.contains(&Kind::RemoteCall);
        let mut queries = Vec::new();
        if calls {
            queries.push("(call) @finding");
        }
        if kinds.contains(&Kind::Binary) {
            queries.push("(binary_operator) @finding");
        } else if calls {
            queries.push("(binary_operator operator: \"|>\") @finding");
        }
        if kinds.contains(&Kind::Unary) {
            queries.push("(unary_operator) @finding");
        }
        queries.join("\n")
    }

    pub(crate) fn candidate<'text, 'tree>(
        node: Node<'tree>,
        source: &'text [u8],
        work: &mut Work,
    ) -> Result<Option<Candidate<'text, 'tree>>> {
        let mut norm = Normalizer { source, work };
        // Verify a pipe only at the outer binary node with its implicit argument.
        let mut parent = node.parent();
        let mut child = node;
        while let Some(outer) = parent {
            norm.work.charge(1)?;
            if outer.kind() == "block" && norm.sole(outer)?.is_some() {
                child = outer;
                parent = outer.parent();
            } else {
                if outer.kind() == "binary_operator"
                    && outer.child_by_field_name("right") == Some(child)
                    && norm.operator(outer)? == "|>"
                {
                    return Ok(None);
                }
                break;
            }
        }
        // All pipes are candidates, but unrelated callees must be rejected before
        // traversing any operands or argument subtrees.
        let mut terminal = norm.unwrap(node)?;
        if terminal.kind() == "binary_operator" && norm.operator(terminal)? == "|>" {
            terminal = norm.unwrap(
                terminal
                    .child_by_field_name("right")
                    .ok_or("missing pipe target")?,
            )?;
        }
        if matches!(terminal.kind(), "unary_operator" | "binary_operator") {
            let token = terminal
                .child_by_field_name("operator")
                .ok_or("missing operator")?;
            return Ok(Some(Candidate {
                node,
                kind: if terminal.kind() == "unary_operator" {
                    Kind::Unary
                } else {
                    Kind::Binary
                },
                name: Cow::Borrowed(norm.text(token)?),
                module: None,
                callee: token.byte_range(),
            }));
        }
        let Some((signature, module)) = norm.signature(terminal)? else {
            return Ok(None);
        };
        let module = if let Some(module) = module {
            let node = norm.unwrap(module)?;
            if matches!(
                node.kind(),
                "alias" | "atom" | "quoted_atom" | "boolean" | "nil"
            ) || (node.kind() == "identifier" && CONTEXT_CONSTANTS.contains(&norm.text(node)?))
            {
                let view = norm.view(Input::Node(node))?;
                Some((view.kind, view.value))
            } else {
                None
            }
        } else {
            None
        };
        Ok(Some(Candidate {
            node,
            kind: signature.kind,
            name: signature.value,
            module,
            callee: signature.callee.ok_or("pattern candidate has no callee")?,
        }))
    }

    pub(crate) fn finding_candidate(
        &self,
        candidate: &Candidate,
        source: &[u8],
        work: &mut Work,
    ) -> Result<Option<Range<usize>>> {
        let mut norm = Normalizer { source, work };
        for index in self
            .alternative_routes
            .candidates(candidate.signature())
            .indices()
        {
            let pattern = &self.alternatives[index];
            if norm.matches(
                pattern,
                Input::Node(candidate.node),
                &mut BTreeMap::new(),
                &self.constraints,
                0,
            )? {
                return Ok(Some(match self.report {
                    PatternReport::Match => candidate.node.byte_range(),
                    PatternReport::Callee | PatternReport::Operator => candidate.callee.clone(),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternatives_compile_to_a_small_shared_candidate_query() {
        let pattern = Pattern::compile(
            &Patterns::Many(vec![
                "Enum.filter(_, fn value -> value != nil end)".into(),
                "Enum.filter(_, fn value -> value !== nil end)".into(),
                "Enum.filter(_, fn value -> nil != value end)".into(),
                "Enum.filter(_, fn value -> !is_nil(value) end)".into(),
            ]),
            PatternReport::Match,
            &BTreeMap::new(),
        )
        .unwrap();
        let (query, _) = crate::engine::compile_query(CANDIDATE_QUERY, true).unwrap();
        assert_eq!(query.pattern_count(), 2);
        assert_eq!(pattern.signatures.len(), 1);
        assert!(CANDIDATE_QUERY.len() < 100);
    }

    #[test]
    fn work_limits_are_checked_without_overflow() {
        let mut work = Work::default();
        assert!(work.charge(MAX_WORK).is_ok());
        assert!(work.charge(1).is_err());
        assert!(work.charge(usize::MAX).is_err());
    }

    #[test]
    fn unrelated_alternatives_do_not_consume_matching_work() {
        let mut alternatives: Vec<_> = (0..256).map(|n| format!("name{n}(_)")).collect();
        alternatives.push("Mod.map(_)".into());
        alternatives.push("receiver.map(_)".into());
        let pattern = Pattern::compile(
            &Patterns::Many(alternatives),
            PatternReport::Callee,
            &BTreeMap::new(),
        )
        .unwrap();
        let mut parser = Parser::new();
        parser.set_language(&crate::engine::language()).unwrap();
        for source in ["name255(value)", "Mod.map(value)", "Other.map(value)"] {
            let tree = crate::scan::parse(&mut parser, source).unwrap();
            let mut work = Work::default();
            let candidate = Pattern::candidate(
                tree.root_node().named_child(0).unwrap(),
                source.as_bytes(),
                &mut work,
            )
            .unwrap()
            .unwrap();
            let before = work.spent;
            assert!(
                pattern
                    .finding_candidate(&candidate, source.as_bytes(), &mut work)
                    .unwrap()
                    .is_some()
            );
            assert!(
                work.spent - before < 100,
                "matching work: {}",
                work.spent - before
            );
        }
    }

    #[test]
    fn duplicate_alternatives_are_shared_and_unrelated_candidates_skip_verification() {
        let pattern = Pattern::compile(
            &Patterns::Many(vec!["foo(_)".into(); 1000]),
            PatternReport::Match,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            (pattern.alternatives.len(), pattern.signatures.len()),
            (1, 1)
        );

        let alternatives = (0..64)
            .map(|n| format!("name{n}{}(_)", "x".repeat(80)))
            .collect();
        let pattern = Pattern::compile(
            &Patterns::Many(alternatives),
            PatternReport::Match,
            &BTreeMap::new(),
        )
        .unwrap();
        let mut parser = Parser::new();
        parser.set_language(&crate::engine::language()).unwrap();
        let source = "seed |> unrelated()";
        let tree = crate::scan::parse(&mut parser, source).unwrap();
        let mut work = Work {
            spent: MAX_WORK - 512,
        };
        let candidate = Pattern::candidate(
            tree.root_node().named_child(0).unwrap(),
            source.as_bytes(),
            &mut work,
        )
        .unwrap()
        .unwrap();
        let before = work.spent;
        assert!(
            pattern
                .finding_candidate(&candidate, source.as_bytes(), &mut work)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            work.spent, before,
            "unrelated alternatives must do no verification"
        );
    }
}
