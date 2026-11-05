//! Bounded capture pipelines. Syntax choices stay in rule data, not in the evaluator.
use crate::Result;
use crate::model::{
    BindingComparison, BindingOperator, QueryCondition, QueryExpectation, QueryRelation, QueryStage,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use tree_sitter::{
    CaptureQuantifier, Node, Query, QueryCursor, QueryCursorOptions, QueryMatch, StreamingIterator,
};
use unicode_normalization::UnicodeNormalization;

struct CompiledCondition {
    capture: usize,
    query: Query,
    relation: QueryRelation,
    expect: QueryExpectation,
}

struct Stage {
    from: usize,
    query: Query,
    unwrap: Vec<u16>,
    ignore: Vec<u16>,
    exports: Vec<(u32, usize)>,
}

struct Comparison {
    left: usize,
    right: usize,
    operator: BindingOperator,
}

#[derive(Default)]
pub(crate) struct Pipeline {
    bindings: BTreeMap<String, usize>,
    stages: Vec<Stage>,
    conditions: Vec<CompiledCondition>,
    comparisons: Vec<Comparison>,
    pub(crate) stage_finding: bool,
    enabled: bool,
}

impl Pipeline {
    pub(crate) fn is_empty(&self) -> bool {
        !self.enabled
    }
}

fn rooted_query(text: &str) -> Result<Query> {
    let (query, _) = crate::engine::compile_query(text, false)?;
    if query.pattern_count() == 0 {
        return Err("query needs at least one pattern".into());
    }
    if (0..query.pattern_count()).any(|pattern| !query.is_pattern_rooted(pattern)) {
        return Err(
            "query patterns must have a single root recognized by tree-sitter; sibling patterns and optimized wildcard-parent roots with children are unsupported; name the parent node kind".into(),
        );
    }
    Ok(query)
}

fn binding(bindings: &BTreeMap<String, usize>, name: &str, field: &str) -> Result<usize> {
    bindings.get(name).copied().ok_or_else(|| {
        format!(
            "{field}: unknown binding {name:?}; use an outer capture or earlier export without @"
        )
    })
}

fn validate_kinds(kinds: &[String], field: &str) -> Result<Vec<u16>> {
    let language = crate::engine::language();
    let mut ids = Vec::new();
    for kind in kinds {
        let previous = ids.len();
        ids.extend((0..language.node_kind_count()).filter_map(|id| {
            (language.node_kind_is_named(id as u16)
                && language.node_kind_for_id(id as u16) == Some(kind))
            .then_some(id as u16)
        }));
        if ids.len() == previous {
            return Err(format!("{field}: unknown named node kind {kind:?}"));
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

pub(crate) fn compile(
    conditions: &[QueryCondition],
    stages: &[QueryStage],
    comparisons: &[BindingComparison],
    outer: &Query,
    errors: &mut Vec<String>,
) -> Pipeline {
    let mut pipeline = Pipeline {
        enabled: !conditions.is_empty() || !stages.is_empty() || !comparisons.is_empty(),
        bindings: outer
            .capture_names()
            .iter()
            .enumerate()
            .map(|(i, name)| ((*name).into(), i))
            .collect(),
        ..Pipeline::default()
    };
    for (index, spec) in stages.iter().enumerate() {
        let result = (|| {
            let from = binding(&pipeline.bindings, &spec.from, "from")?;
            let unwrap = validate_kinds(&spec.unwrap_singleton, "unwrap_singleton")?;
            let ignore = validate_kinds(&spec.ignore_children, "ignore_children")?;
            let query = rooted_query(&spec.query).map_err(|error| format!("query: {error}"))?;
            let mut exports = Vec::new();
            let mut names = pipeline.bindings.clone();
            for (i, name) in spec.export.iter().enumerate() {
                if names.contains_key(name) {
                    return Err(format!("export[{i}]: binding {name:?} already exists"));
                }
                let capture = query
                    .capture_index_for_name(name)
                    .ok_or_else(|| format!("export[{i}]: unknown local capture {name:?}"))?;
                if (0..query.pattern_count()).any(|pattern| {
                    query.capture_quantifiers(pattern)[capture as usize] != CaptureQuantifier::One
                }) {
                    return Err(format!(
                        "export[{i}]: capture {name:?} must occur exactly once in every query pattern"
                    ));
                }
                let slot = names.len();
                names.insert(name.clone(), slot);
                exports.push((capture, slot));
            }
            Ok((
                Stage {
                    from,
                    query,
                    unwrap,
                    ignore,
                    exports,
                },
                names,
            ))
        })();
        match result {
            Ok((stage, bindings)) => {
                pipeline.stages.push(stage);
                pipeline.bindings = bindings;
            }
            Err(error) => errors.push(format!("stages[{index}].{error}")),
        }
    }
    for (index, spec) in comparisons.iter().enumerate() {
        let result: Result<Comparison> = (|| {
            Ok(Comparison {
                left: binding(&pipeline.bindings, &spec.left, "left")?,
                right: binding(&pipeline.bindings, &spec.right, "right")?,
                operator: spec.operator,
            })
        })();
        match result {
            Ok(comparison) => pipeline.comparisons.push(comparison),
            Err(error) => errors.push(format!("comparisons[{index}].{error}")),
        }
    }
    for (index, spec) in conditions.iter().enumerate() {
        let result: Result<CompiledCondition> = (|| {
            let capture = pipeline.bindings.get(&spec.capture).copied().ok_or_else(|| {
                format!("capture: unknown outer capture or stage export {:?} (use its name without @)", spec.capture)
            })?;
            let query = rooted_query(&spec.query).map_err(|error| format!("query: {error}"))?;
            Ok(CompiledCondition {
                capture,
                query,
                relation: spec.relation,
                expect: spec.expect,
            })
        })();
        match result {
            Ok(condition) => pipeline.conditions.push(condition),
            Err(error) => errors.push(format!("conditions[{index}].{error}")),
        }
    }
    if !pipeline.bindings.contains_key("finding") {
        errors.push("stages: final binding `finding` is required; capture it in the outer query or export it from a stage".into());
    }
    pipeline.stage_finding = outer.capture_index_for_name("finding").is_none()
        && pipeline.bindings.contains_key("finding");
    pipeline
}

pub(crate) enum Outcome<'tree> {
    Reject,
    Outer,
    Finding(Node<'tree>),
}

// Memoized scopes and path-compressed wrappers avoid repeated traversal. Conservative
// work limits bound distinct overlapping scopes too; these are errors, not truncation.
const MAX_SCOPE_NODES: usize = 10_000_000;
const MAX_STAGE_STEPS: usize = 100_000;
const MAX_QUERY_CANDIDATES: usize = 100_000;
const MAX_PREDICATE_WORK: usize = 1_000_000;
const MAX_NATIVE_PROGRESS: usize = 100_000;
const QUERY_DEADLINE: Duration = Duration::from_secs(2);
const FILE_QUERY_TIME: Duration = Duration::from_secs(5);

struct Progress {
    deadline: Instant,
    remaining: Cell<usize>,
    cancelled: Cell<Option<&'static str>>,
}

impl Progress {
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            self.cancelled
                .set(Some("auxiliary query execution deadline exceeded"));
        }
        self.cancelled
            .get()
            .map_or(Ok(()), |error| Err(error.into()))
    }

    fn callback(&self) -> bool {
        if let Some(remaining) = self.remaining.get().checked_sub(1) {
            self.remaining.set(remaining);
        } else {
            self.cancelled
                .set(Some("auxiliary query native progress limit exceeded"));
        }
        self.check().is_err()
    }
}

struct QueryWork {
    candidates: usize,
    predicates: usize,
    native: usize,
    active_time: Duration,
    per_query_time: Duration,
    child_counts: HashMap<usize, usize>,
}

impl Default for QueryWork {
    fn default() -> Self {
        Self {
            candidates: MAX_QUERY_CANDIDATES,
            predicates: MAX_PREDICATE_WORK,
            native: MAX_NATIVE_PROGRESS,
            active_time: FILE_QUERY_TIME,
            per_query_time: QUERY_DEADLINE,
            child_counts: HashMap::new(),
        }
    }
}

impl QueryWork {
    fn predicate_work(&mut self, amount: usize) -> Result<()> {
        self.predicates = self
            .predicates
            .checked_sub(amount)
            .ok_or("auxiliary query predicate work limit exceeded")?;
        Ok(())
    }

    fn child_count(&mut self, node: Node, progress: &Progress) -> Result<usize> {
        if let Some(&count) = self.child_counts.get(&node.id()) {
            return Ok(count);
        }
        let mut count = 0;
        let mut walk = node.walk();
        for (index, child) in node.named_children(&mut walk).enumerate() {
            self.predicate_work(1)?;
            if index % 256 == 0 {
                progress.check()?;
            }
            count += usize::from(child.kind() != "comment");
        }
        self.child_counts.insert(node.id(), count);
        Ok(count)
    }

    fn predicates_hold(
        &mut self,
        query: &Query,
        found: &QueryMatch,
        progress: &Progress,
    ) -> Result<bool> {
        use tree_sitter::QueryPredicateArg::{Capture, String as Text};
        for predicate in query.general_predicates(found.pattern_index) {
            self.predicate_work(1)?;
            let [Capture(index), Text(n)] = predicate.args.as_ref() else {
                return Ok(false);
            };
            let n = n.parse().unwrap_or(usize::MAX);
            // Filtering captures inspects all of them, including non-matching captures.
            self.predicate_work(found.captures.len())?;
            for capture in found
                .captures
                .iter()
                .filter(|capture| capture.index == *index)
            {
                let count = self.child_count(capture.node, progress)?;
                let passes = match predicate.operator.as_ref() {
                    "count-eq?" => count == n,
                    "count-gt?" => count > n,
                    "count-lt?" => count < n,
                    _ => false,
                };
                if !passes {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

struct Search<'query, 'tree, 'text> {
    query: &'query Query,
    node: Node<'tree>,
    text: &'text [u8],
    root_only: bool,
}

impl<'tree> Search<'_, 'tree, '_> {
    fn run(
        &self,
        cursor: &mut QueryCursor,
        work: &mut QueryWork,
        mut visit: impl FnMut(&QueryMatch<'_, 'tree>) -> bool,
    ) -> Result<()> {
        let started = Instant::now();
        let progress = Progress {
            deadline: started + work.active_time.min(work.per_query_time),
            remaining: Cell::new(work.native),
            cancelled: Cell::new(None),
        };
        let mut cancel = |_: &tree_sitter::QueryCursorState| progress.callback();
        cursor.set_max_start_depth(self.root_only.then_some(0));
        let mut matches = cursor.matches_with_options(
            self.query,
            self.node,
            self.text,
            QueryCursorOptions::new().progress_callback(&mut cancel),
        );
        let result = (|| {
            progress.check()?;
            while let Some(found) = matches.next() {
                progress.check()?;
                work.candidates = work
                    .candidates
                    .checked_sub(1)
                    .ok_or("auxiliary query candidate limit exceeded")?;
                if work.predicates_hold(self.query, found, &progress)? && visit(found) {
                    break;
                }
            }
            progress.check()
        })();
        drop(matches);
        work.native = progress.remaining.get();
        work.active_time = work.active_time.saturating_sub(started.elapsed());
        // Cancellation or overflow cannot establish absence, even after a witness.
        progress.check()?;
        if cursor.did_exceed_match_limit() {
            return Err("query match limit exceeded".into());
        }
        result
    }
}

#[derive(Clone, Copy, Default)]
enum Binding<'tree> {
    #[default]
    Missing,
    One(Node<'tree>),
    Repeated,
}

impl<'tree> Binding<'tree> {
    fn node(self) -> Option<Node<'tree>> {
        if let Self::One(node) = self {
            Some(node)
        } else {
            None
        }
    }
}

pub(crate) struct Evaluator<'tree> {
    cursor: Option<QueryCursor>,
    match_limit: u32,
    cache: HashMap<(usize, usize), bool>,
    wrappers: HashMap<(usize, usize), Node<'tree>>,
    stages: HashMap<(usize, usize), Option<Vec<Node<'tree>>>>,
    remaining: usize,
    stage_remaining: usize,
    work: QueryWork,
}

impl<'tree> Evaluator<'tree> {
    pub(crate) fn new(match_limit: u32) -> Self {
        Self {
            cursor: None,
            match_limit,
            cache: HashMap::new(),
            wrappers: HashMap::new(),
            stages: HashMap::new(),
            remaining: MAX_SCOPE_NODES,
            stage_remaining: MAX_STAGE_STEPS,
            work: QueryWork::default(),
        }
    }

    fn prepare_cursor(&mut self) {
        self.cursor.get_or_insert_with(|| {
            let mut cursor = QueryCursor::new();
            cursor.set_match_limit(self.match_limit);
            cursor
        });
    }

    fn charge(&mut self, nodes: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(nodes)
            .ok_or("capture pipeline scope work limit exceeded")?;
        Ok(())
    }

    fn unwrap(&mut self, stage: &Stage, mut node: Node<'tree>) -> Result<Node<'tree>> {
        let profile = std::ptr::from_ref(stage) as usize;
        let mut path = Vec::new();
        loop {
            if let Some(&cached) = self.wrappers.get(&(profile, node.id())) {
                node = cached;
                break;
            }
            self.charge(1)?;
            path.push(node.id());
            if !stage.unwrap.contains(&node.kind_id()) {
                break;
            }
            let mut walk = node.walk();
            let mut child = None;
            let mut multiple = false;
            for candidate in node.named_children(&mut walk) {
                self.charge(1)?;
                if stage.ignore.contains(&candidate.kind_id()) {
                    continue;
                }
                if child.replace(candidate).is_some() {
                    multiple = true;
                    break;
                }
            }
            match (child, multiple) {
                (Some(single), false) => node = single,
                _ => break,
            }
        }
        for id in path {
            self.wrappers.insert((profile, id), node);
        }
        Ok(node)
    }

    fn stage(
        &mut self,
        stage: &Stage,
        node: Node<'tree>,
        text: &[u8],
    ) -> Result<Option<Vec<Node<'tree>>>> {
        self.stage_remaining = self
            .stage_remaining
            .checked_sub(1)
            .ok_or("capture pipeline stage step limit exceeded")?;
        let node = self.unwrap(stage, node)?;
        let key = (std::ptr::from_ref(stage) as usize, node.id());
        if let Some(cached) = self.stages.get(&key) {
            return Ok(cached.clone());
        }
        self.charge(node.descendant_count())?;
        self.prepare_cursor();
        let mut result = None;
        let mut ambiguous = false;
        let mut invalid_exports = false;
        Search {
            query: &stage.query,
            node,
            text,
            root_only: true,
        }
        .run(
            self.cursor.as_mut().expect("prepared cursor"),
            &mut self.work,
            |found| {
                if result.is_some() {
                    ambiguous = true;
                    return true;
                }
                let mut exports = Vec::new();
                for (capture, _) in &stage.exports {
                    let mut nodes = found.captures.iter().filter(|item| item.index == *capture);
                    let Some(first) = nodes.next() else {
                        invalid_exports = true;
                        break;
                    };
                    if nodes.next().is_some() {
                        invalid_exports = true;
                        break;
                    }
                    exports.push(first.node);
                }
                if invalid_exports {
                    return true;
                }
                result = Some(exports);
                false
            },
        )?;
        if ambiguous {
            return Err(
                "query produced multiple matches; root stages require exactly one match".into(),
            );
        }
        if invalid_exports {
            return Ok(None);
        }
        self.stages.insert(key, result.clone());
        Ok(result)
    }

    pub(crate) fn evaluate(
        &mut self,
        pipeline: &Pipeline,
        outer: &Query,
        found: &QueryMatch<'_, 'tree>,
        text: &[u8],
    ) -> Result<Outcome<'tree>> {
        if pipeline.is_empty() {
            return Ok(Outcome::Outer);
        }
        let mut bindings = vec![Binding::Missing; pipeline.bindings.len()];
        for capture in found.captures {
            // Capture indices differ between strict and grouped queries; names are stable.
            if let Some(&slot) = pipeline
                .bindings
                .get(outer.capture_names()[capture.index as usize])
            {
                bindings[slot] = match bindings[slot] {
                    Binding::Missing => Binding::One(capture.node),
                    _ => Binding::Repeated,
                };
            }
        }
        for (index, stage) in pipeline.stages.iter().enumerate() {
            let Some(node) = bindings[stage.from].node() else {
                return Ok(Outcome::Reject);
            };
            let Some(exports) = self
                .stage(stage, node, text)
                .map_err(|error| format!("stages[{index}]: {error}"))?
            else {
                return Ok(Outcome::Reject);
            };
            for ((_, slot), node) in stage.exports.iter().zip(exports) {
                bindings[*slot] = Binding::One(node);
            }
        }
        for comparison in &pipeline.comparisons {
            let (Some(left), Some(right)) = (
                bindings[comparison.left].node(),
                bindings[comparison.right].node(),
            ) else {
                return Ok(Outcome::Reject);
            };
            let left = &text[left.byte_range()];
            let right = &text[right.byte_range()];
            let equal = match comparison.operator {
                BindingOperator::TextEq | BindingOperator::TextNotEq => left == right,
                BindingOperator::NfcEq | BindingOperator::NfcNotEq => {
                    self.work.predicate_work(left.len())?;
                    self.work.predicate_work(right.len())?;
                    if left == right {
                        true
                    } else if left.is_ascii() && right.is_ascii() {
                        false
                    } else {
                        let left = std::str::from_utf8(left)
                            .map_err(|error| format!("normalized comparison: {error}"))?;
                        let right = std::str::from_utf8(right)
                            .map_err(|error| format!("normalized comparison: {error}"))?;
                        left.nfc().eq(right.nfc())
                    }
                }
            };
            if !match comparison.operator {
                BindingOperator::TextEq | BindingOperator::NfcEq => equal,
                BindingOperator::TextNotEq | BindingOperator::NfcNotEq => !equal,
            } {
                return Ok(Outcome::Reject);
            }
        }
        for condition in &pipeline.conditions {
            let Some(node) = bindings[condition.capture].node() else {
                return Ok(Outcome::Reject);
            };
            let key = (std::ptr::from_ref(condition) as usize, node.id());
            let witness = if let Some(&witness) = self.cache.get(&key) {
                witness
            } else {
                self.charge(node.descendant_count())?;
                self.prepare_cursor();
                let mut witness = false;
                Search {
                    query: &condition.query,
                    node,
                    text,
                    root_only: matches!(condition.relation, QueryRelation::Root),
                }
                .run(
                    self.cursor.as_mut().expect("prepared cursor"),
                    &mut self.work,
                    |_| {
                        witness = true;
                        true
                    },
                )
                .map_err(|error| format!("capture condition {error}"))?;
                self.cache.insert(key, witness);
                witness
            };
            if !match condition.expect {
                QueryExpectation::Exists => witness,
                QueryExpectation::Absent => !witness,
            } {
                return Ok(Outcome::Reject);
            }
        }
        if pipeline.stage_finding {
            Ok(match bindings[pipeline.bindings["finding"]].node() {
                Some(node) => Outcome::Finding(node),
                None => Outcome::Reject,
            })
        } else {
            Ok(Outcome::Outer)
        }
    }

    #[cfg(test)]
    fn holds(
        &mut self,
        pipeline: &Pipeline,
        outer: &Query,
        found: &QueryMatch<'_, 'tree>,
        text: &[u8],
    ) -> Result<bool> {
        self.evaluate(pipeline, outer, found, text)
            .map(|outcome| !matches!(outcome, Outcome::Reject))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_candidates_are_counted_and_child_inspections_are_cached() {
        let text = format!("[{}]\n", vec!["item"; 80].join(", "));
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, &text).unwrap();
        let node = tree.root_node().named_child(0).unwrap();
        let query = rooted_query("((list (identifier) @a (identifier) @b) @xs (#count-eq? @xs 0))")
            .unwrap();
        let mut work = QueryWork {
            candidates: 8,
            ..QueryWork::default()
        };
        let error = Search {
            query: &query,
            node,
            text: text.as_bytes(),
            root_only: true,
        }
        .run(&mut cursor, &mut work, |_| {
            panic!("all candidates fail the count predicate")
        })
        .unwrap_err();
        assert!(error.contains("candidate limit exceeded"), "{error}");
        assert!(!cursor.did_exceed_match_limit());
        assert_eq!(work.child_counts.len(), 1);
        assert_eq!(work.child_counts[&node.id()], 80);
        assert_eq!(work.predicates, MAX_PREDICATE_WORK - 80 - 8 * 4);
    }

    #[test]
    fn predicate_work_exhaustion_does_not_cache_a_partial_child_count() {
        let text = format!("[{}]\n", vec!["item"; 30].join(", "));
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, &text).unwrap();
        let query = rooted_query("((list) @xs (#count-eq? @xs 0))").unwrap();
        let mut work = QueryWork {
            predicates: 12,
            ..QueryWork::default()
        };
        let error = Search {
            query: &query,
            node: tree.root_node(),
            text: text.as_bytes(),
            root_only: false,
        }
        .run(&mut cursor, &mut work, |_| false)
        .unwrap_err();
        assert!(error.contains("predicate work limit exceeded"), "{error}");
        assert!(work.child_counts.is_empty());
        assert!(!cursor.did_exceed_match_limit());
    }

    #[test]
    fn native_progress_cancels_queries_even_when_text_predicates_hide_all_candidates() {
        let text = format!("[{}]\n", vec!["item"; 100].join(", "));
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, &text).unwrap();
        let query =
            rooted_query("((list (identifier) @a (identifier) @b) (#eq? @a \"never\"))").unwrap();
        let mut work = QueryWork {
            native: 0,
            ..QueryWork::default()
        };
        let error = Search {
            query: &query,
            node: tree.root_node(),
            text: text.as_bytes(),
            root_only: false,
        }
        .run(&mut cursor, &mut work, |_| {
            panic!("no text predicate can pass")
        })
        .unwrap_err();
        assert!(error.contains("native progress limit exceeded"), "{error}");
        assert_eq!(work.candidates, MAX_QUERY_CANDIDATES);
        assert!(!cursor.did_exceed_match_limit());
    }

    #[test]
    fn expired_deadlines_fail_closed_in_the_progress_callback_and_before_search() {
        let progress = Progress {
            deadline: Instant::now(),
            remaining: Cell::new(10),
            cancelled: Cell::new(None),
        };
        assert!(progress.callback());
        assert!(
            progress
                .check()
                .unwrap_err()
                .contains("execution deadline exceeded")
        );
        let text = "[item]\n";
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, text).unwrap();
        let query = rooted_query("(list)").unwrap();
        let mut work = QueryWork {
            active_time: Duration::ZERO,
            ..QueryWork::default()
        };
        let error = Search {
            query: &query,
            node: tree.root_node(),
            text: text.as_bytes(),
            root_only: false,
        }
        .run(&mut cursor, &mut work, |_| {
            panic!("expired queries cannot yield witnesses")
        })
        .unwrap_err();
        assert!(error.contains("execution deadline exceeded"), "{error}");
    }

    #[test]
    fn empty_pipelines_use_no_environment_or_cache_work() {
        let text = "x\n";
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, text).unwrap();
        let (outer, _) = crate::engine::compile_query("(identifier) @finding", true).unwrap();
        let mut matches = cursor.matches(&outer, tree.root_node(), text.as_bytes());
        let found = matches.next().unwrap();
        let mut evaluator = Evaluator::new(16_384);
        evaluator.remaining = 0;
        evaluator.stage_remaining = 0;
        assert!(matches!(
            evaluator
                .evaluate(&Pipeline::default(), &outer, found, text.as_bytes())
                .unwrap(),
            Outcome::Outer
        ));
        assert!(evaluator.cache.is_empty());
        assert!(evaluator.wrappers.is_empty());
        assert!(evaluator.stages.is_empty());
        assert!(evaluator.cursor.is_none());
    }

    #[test]
    fn repeated_scope_results_are_cached_and_new_scope_work_is_bounded() {
        let text = "[x]\n[y]\n";
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, text).unwrap();
        let (outer, _) = crate::engine::compile_query("(list) @finding @_scope", true).unwrap();
        let specs = [QueryCondition {
            capture: "_scope".into(),
            query: "(identifier)".into(),
            relation: QueryRelation::DescendantOrSelf,
            expect: QueryExpectation::Exists,
        }];
        let mut errors = Vec::new();
        let pipeline = compile(&specs, &[], &[], &outer, &mut errors);
        assert!(errors.is_empty());
        let mut evaluator = Evaluator::new(16_384);
        let mut matches = cursor.matches(&outer, tree.root_node(), text.as_bytes());
        let first = matches.next().unwrap();
        assert!(
            evaluator
                .holds(&pipeline, &outer, first, text.as_bytes())
                .unwrap()
        );
        evaluator.remaining = 0;
        assert!(
            evaluator
                .holds(&pipeline, &outer, first, text.as_bytes())
                .unwrap()
        );
        assert_eq!(evaluator.cache.len(), 1);
        let second = matches.next().unwrap();
        assert!(
            evaluator
                .holds(&pipeline, &outer, second, text.as_bytes())
                .unwrap_err()
                .contains("scope work limit exceeded")
        );
    }

    #[test]
    fn wrapper_paths_and_stage_results_are_cached_and_limits_fail_closed() {
        let text = "[[[x]]]\n";
        let (mut parser, mut cursor) = crate::scan::worker();
        let tree = crate::scan::parse(&mut parser, text).unwrap();
        let (outer, _) = crate::engine::compile_query("(list) @finding @_scope", true).unwrap();
        let stage: QueryStage = toml::from_str("from = '_scope'\nunwrap_singleton = ['list']\nquery = '(identifier) @item'\nexport = ['item']\n").unwrap();
        let mut errors = Vec::new();
        let pipeline = compile(&[], &[stage], &[], &outer, &mut errors);
        assert!(errors.is_empty());
        let mut evaluator = Evaluator::new(16_384);
        let mut matches = cursor.matches(&outer, tree.root_node(), text.as_bytes());
        let first = matches.next().unwrap();
        assert!(
            evaluator
                .holds(&pipeline, &outer, first, text.as_bytes())
                .unwrap()
        );
        assert_eq!(evaluator.wrappers.len(), 4);
        assert_eq!(evaluator.stages.len(), 1);
        evaluator.remaining = 0;
        let second = matches.next().unwrap();
        assert!(
            evaluator
                .holds(&pipeline, &outer, second, text.as_bytes())
                .unwrap()
        );
        assert_eq!(evaluator.stages.len(), 1);
        evaluator.stage_remaining = 0;
        assert!(
            evaluator
                .holds(&pipeline, &outer, second, text.as_bytes())
                .unwrap_err()
                .contains("stage step limit exceeded")
        );
        let mut exhausted = Evaluator::new(16_384);
        exhausted.remaining = 0;
        assert!(
            exhausted
                .holds(&pipeline, &outer, second, text.as_bytes())
                .unwrap_err()
                .contains("scope work limit exceeded")
        );
    }
}
