use crate::directives::Directives;
use crate::engine::{Cond, Matcher, Rule};
use crate::mask::{self, Layer, Regions, content_range};
use crate::model::{Limit, Scope};
use crate::snapshot::{entries, valid_relative_path};
use crate::{Diagnostic, Engine, Finding, Report, Result, Snapshot};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tree_sitter::{ParseOptions, ParseState, Parser, QueryCursor, StreamingIterator, Tree};

const MAX_FINDINGS: usize = 100_000;
const PARSE_BUDGET: Duration = Duration::from_secs(10);
const DEFAULT_LINK_LIMIT: u32 = 8;
const RETRY_LINK_LIMIT: u32 = 64;

unsafe extern "C" {
    // Vendored runtime: per-thread parse-stack link limit (upstream fixes it at 8).
    fn ts_beam_set_stack_link_limit(limit: u32);
}

fn parse_once(parser: &mut Parser, text: &str) -> Option<Tree> {
    let start = Instant::now();
    let mut cancel = |_: &ParseState| start.elapsed() > PARSE_BUDGET;
    let bytes = text.as_bytes();
    let mut input = |offset: usize, _| bytes.get(offset..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut cancel)),
    );
    if tree.is_none() {
        parser.reset();
    }
    tree
}

/// Parse with upstream limits. Only if that yields a syntax error, retry once with a
/// higher but finite stack link limit: some valid, highly ambiguous Elixir needs more
/// than eight parse-stack links. Every attempt has a time budget and fails closed.
pub(crate) fn parse(parser: &mut Parser, text: &str) -> std::result::Result<Tree, String> {
    let budget = || {
        format!(
            "parser exceeded its {}s time budget",
            PARSE_BUDGET.as_secs()
        )
    };
    let tree = parse_once(parser, text).ok_or_else(budget)?;
    if !tree.root_node().has_error() {
        return Ok(tree);
    }
    // SAFETY: plain setter for a thread-local integer in the linked runtime.
    unsafe { ts_beam_set_stack_link_limit(RETRY_LINK_LIMIT) };
    let retry = parse_once(parser, text);
    unsafe { ts_beam_set_stack_link_limit(DEFAULT_LINK_LIMIT) };
    retry.ok_or_else(budget)
}

pub(crate) fn run(
    engine: &Engine,
    root: &Path,
    explicit: Option<&[String]>,
    threads: usize,
    snapshot: &Snapshot,
) -> Result<Report> {
    let start = Instant::now();
    if threads == 0 {
        return Err("threads must be greater than zero".into());
    }
    let root = fs::canonicalize(root).map_err(|e| format!("invalid scan root: {e}"))?;
    if !root.is_dir() {
        return Err("scan root must be a directory".into());
    }
    let mut paths = BTreeSet::new();
    match explicit {
        Some(files) => {
            for file in files {
                if !valid_relative_path(file) {
                    return Err(format!("expected a relative path, got {file:?}"));
                }
                if !elixir(Path::new(file)) || !selected(engine, file) {
                    continue;
                }
                let actual = fs::canonicalize(root.join(file))
                    .map_err(|e| format!("cannot resolve {file}: {e}"))?;
                if !actual.starts_with(&root) || !actual.is_file() {
                    return Err(format!("path escapes scan root or is not a file: {file}"));
                }
                if selected(engine, file) {
                    paths.insert(file.clone());
                }
            }
        }
        None => {
            let excluded = engine.exclude.clone();
            let filter_root = root.clone();
            let mut walker = WalkBuilder::new(&root);
            walker
                .hidden(false)
                .parents(false)
                .git_global(false)
                .require_git(false)
                .follow_links(false)
                .filter_entry(move |entry| {
                    if entry.depth() == 0 {
                        return true;
                    }
                    if entry.file_type().is_some_and(|t| t.is_dir())
                        && matches!(
                            entry.file_name().to_str(),
                            Some(".git" | "_build" | "deps" | "node_modules" | "target")
                        )
                    {
                        return false;
                    }
                    entry
                        .path()
                        .strip_prefix(&filter_root)
                        .ok()
                        .is_some_and(|path| {
                            !path_excluded(&excluded, path)
                                && !excluded.is_match(format!("{}/", path.display()))
                        })
                });
            for entry in walker.build() {
                let entry = entry.map_err(|e| format!("file discovery failed: {e}"))?;
                if let Some(error) = entry.error() {
                    return Err(format!("ignore policy could not be read: {error}"));
                }
                if !entry.file_type().is_some_and(|t| t.is_file()) || !elixir(entry.path()) {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&root)
                    .map_err(|e| e.to_string())?;
                let file = relative.to_str().ok_or("non-UTF8 source path")?.to_string();
                if selected(engine, &file) {
                    paths.insert(file);
                }
            }
        }
    }
    if paths.is_empty() && explicit.is_none() {
        return Err("no Elixir files selected; refusing an empty scan".into());
    }
    let paths: Vec<_> = paths.into_iter().collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| e.to_string())?;
    let scanned: Vec<_> = pool.install(|| {
        paths
            .par_iter()
            .map_init(worker, |(parser, cursor), file| {
                let text = fs::read_to_string(root.join(file)).map_err(|e| {
                    diagnostic(file, 1, 1, format!("cannot read UTF-8 source: {e}"))
                })?;
                source(engine, None, parser, cursor, file, &text).map(|rows| {
                    let current = entries(&rows, &text);
                    (rows, current)
                })
            })
            .collect()
    });
    let mut findings = Vec::new();
    let mut errors = Vec::new();
    let mut current = BTreeSet::new();
    for result in scanned {
        match result {
            Ok((rows, entries)) => {
                findings.extend(rows);
                current.extend(entries);
            }
            Err(error) => errors.push(error),
        }
    }
    findings.sort();
    findings.dedup();
    errors.sort();
    let new: Vec<_> = current.difference(&snapshot.0).cloned().collect();
    let active: BTreeSet<_> = engine.rule_ids().collect();
    let stale = snapshot
        .0
        .difference(&current)
        .filter(|e| {
            active.contains(e.rule.as_str())
                && if explicit.is_some() {
                    paths.binary_search(&e.file).is_ok()
                } else {
                    paths.binary_search(&e.file).is_ok()
                        || (elixir(Path::new(&e.file))
                            && selected(engine, &e.file)
                            && matches!(root.join(&e.file).try_exists(), Ok(false)))
                }
        })
        .cloned()
        .collect();
    let passed = errors.is_empty() && new.is_empty();
    Ok(Report {
        files: paths.len(),
        findings,
        errors,
        current,
        new,
        stale,
        scope: if explicit.is_some() { "subset" } else { "full" }.into(),
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        passed,
    })
}

fn selected(engine: &Engine, file: &str) -> bool {
    engine.include.is_match(file) && !path_excluded(&engine.exclude, Path::new(file))
}

fn path_excluded(excluded: &globset::GlobSet, path: &Path) -> bool {
    excluded.is_match(path)
        || path
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .filter(|parent| !parent.as_os_str().is_empty())
            .any(|parent| {
                excluded.is_match(parent) || excluded.is_match(format!("{}/", parent.display()))
            })
}

fn elixir(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ex" | "exs")
    )
}

pub(crate) fn worker() -> (Parser, QueryCursor) {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_elixir::LANGUAGE.into())
        .expect("pinned grammar ABI");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(16_384);
    (parser, cursor)
}

fn diagnostic(file: &str, line: usize, column: usize, message: String) -> Diagnostic {
    Diagnostic {
        file: file.into(),
        line,
        column,
        message,
    }
}

/// Evaluate the engine's #count-* predicates (tree-sitter evaluates text predicates).
fn predicates_hold(query: &tree_sitter::Query, found: &tree_sitter::QueryMatch) -> bool {
    use tree_sitter::QueryPredicateArg::{Capture, String as Text};
    query
        .general_predicates(found.pattern_index)
        .iter()
        .all(|predicate| {
            let (Some(Capture(capture)), Some(Text(n))) =
                (predicate.args.first(), predicate.args.get(1))
            else {
                return false;
            };
            let n: usize = n.parse().unwrap_or(usize::MAX);
            found
                .captures
                .iter()
                .filter(|c| c.index == *capture)
                .all(|c| {
                    let mut walk = c.node.walk();
                    let count = c
                        .node
                        .named_children(&mut walk)
                        .filter(|child| child.kind() != "comment")
                        .count();
                    match predicate.operator.as_ref() {
                        "count-eq?" => count == n,
                        "count-gt?" => count > n,
                        "count-lt?" => count < n,
                        _ => false,
                    }
                })
        })
}

fn record(engine: &Engine, slot: usize, started: Instant) {
    if let Some(total) = engine.timings.get(slot) {
        total.fetch_add(
            started.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
        Self { text, starts }
    }
    /// Zero-based line index containing `byte`.
    fn index(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s <= byte) - 1
    }
    fn line(&self, index: usize) -> &'a str {
        let end = self
            .starts
            .get(index + 1)
            .map_or(self.text.len(), |e| e - 1);
        &self.text[self.starts[index]..end]
    }
    fn count(&self) -> usize {
        self.starts.len()
    }
}

/// One file's parse tree plus lazily built masks and directives.
struct FileCtx<'a> {
    text: &'a str,
    tree: tree_sitter::Tree,
    lines: Lines<'a>,
    regions: Option<Regions>,
    masked: HashMap<String, String>,
}

impl FileCtx<'_> {
    /// The text a regex rule matches against: masked per the rule, cached per rule.
    fn subject(&mut self, rule: &Rule, cursor: &mut QueryCursor) -> Result<String> {
        if rule.masks.is_empty() {
            return Ok(self.text.to_string());
        }
        if let Some(found) = self.masked.get(&rule.id) {
            return Ok(found.clone());
        }
        let regions = self
            .regions
            .get_or_insert_with(|| Regions::collect(&self.tree, self.text.as_bytes()));
        let mut layers = Vec::new();
        for mask in &rule.masks {
            let mut ranges: Vec<_> = mask
                .nodes
                .iter()
                .flat_map(|n| regions.get(*n).iter().cloned())
                .collect();
            if let Some(query) = &mask.query {
                let named: Vec<bool> = query
                    .capture_names()
                    .iter()
                    .map(|n| !n.starts_with('_'))
                    .collect();
                let mut matches =
                    cursor.matches(query, self.tree.root_node(), self.text.as_bytes());
                while let Some(found) = matches.next() {
                    if !predicates_hold(query, found) {
                        continue;
                    }
                    for capture in found.captures.iter().filter(|c| named[c.index as usize]) {
                        ranges.push(content_range(capture.node, self.text.as_bytes()));
                    }
                }
                drop(matches);
                check_query_limit(cursor)?;
            }
            layers.push(Layer {
                ranges,
                with: mask.with,
                fill: mask.fill,
            });
        }
        let masked = mask::apply(self.text, &layers);
        self.masked.insert(rule.id.clone(), masked.clone());
        Ok(masked)
    }
}

fn query_lines(
    cursor: &mut QueryCursor,
    query: &tree_sitter::Query,
    ctx: &FileCtx,
) -> Result<BTreeSet<usize>> {
    let names = query.capture_names();
    let named: Vec<bool> = names.iter().map(|n| !n.starts_with('_')).collect();
    let any_named = named.iter().any(|&n| n);
    let mut lines = BTreeSet::new();
    let mut matches = cursor.matches(query, ctx.tree.root_node(), ctx.text.as_bytes());
    while let Some(found) = matches.next() {
        if !predicates_hold(query, found) {
            continue;
        }
        for capture in found.captures {
            if !any_named || named[capture.index as usize] {
                lines.insert(capture.node.start_position().row + 1);
            }
        }
    }
    drop(matches);
    check_query_limit(cursor)?;
    Ok(lines)
}

fn capture_ranges(
    cursor: &mut QueryCursor,
    query: &tree_sitter::Query,
    ctx: &FileCtx,
) -> Result<Vec<std::ops::Range<usize>>> {
    let named: Vec<bool> = query
        .capture_names()
        .iter()
        .map(|n| !n.starts_with('_'))
        .collect();
    let mut ranges = Vec::new();
    let mut matches = cursor.matches(query, ctx.tree.root_node(), ctx.text.as_bytes());
    while let Some(found) = matches.next() {
        if !predicates_hold(query, found) {
            continue;
        }
        for capture in found.captures.iter().filter(|c| named[c.index as usize]) {
            ranges.push(capture.node.byte_range());
        }
    }
    drop(matches);
    check_query_limit(cursor)?;
    Ok(ranges)
}

fn file_condition(
    cursor: &mut QueryCursor,
    cond: &Cond,
    ctx: &FileCtx,
) -> std::result::Result<bool, String> {
    if let Some(regex) = &cond.regex
        && regex.is_match(ctx.text).map_err(|e| e.to_string())?
    {
        return Ok(true);
    }
    if let Some(query) = &cond.query {
        let mut matches = cursor.matches(query, ctx.tree.root_node(), ctx.text.as_bytes());
        while let Some(found) = matches.next() {
            if predicates_hold(query, found) {
                // A valid witness establishes existence; later matches cannot change it.
                // Still reject any overflow encountered while finding that witness.
                drop(matches);
                check_query_limit(cursor)?;
                return Ok(true);
            }
        }
        drop(matches);
        check_query_limit(cursor)?;
        return Ok(false);
    }
    Ok(false)
}

fn check_query_limit(cursor: &QueryCursor) -> Result<()> {
    if cursor.did_exceed_match_limit() {
        Err("auxiliary query match limit exceeded".into())
    } else {
        Ok(())
    }
}

/// Candidate finding: zero-based line, byte offset of the line start in the original
/// text plus the column offset, and the end byte.
struct Hit {
    line: usize,
    column: usize,
    start: usize,
    end: usize,
}

fn rule_hits(
    rule: &Rule,
    cursor: &mut QueryCursor,
    ctx: &mut FileCtx,
    shared: Option<Vec<Hit>>,
) -> std::result::Result<Vec<Hit>, String> {
    let mut hits = Vec::new();
    match &rule.matcher {
        Matcher::Query { .. } if shared.is_some() => hits = shared.unwrap_or_default(),
        Matcher::PathOnly => hits.push(Hit {
            line: 0,
            column: 0,
            start: 0,
            end: 0,
        }),
        Matcher::Query { query, capture } => {
            let Some(query) = query else {
                return Err(format!("query for {} was not compiled", rule.id));
            };
            {
                let mut matches = cursor.matches(query, ctx.tree.root_node(), ctx.text.as_bytes());
                while let Some(found) = matches.next() {
                    if !predicates_hold(query, found) {
                        continue;
                    }
                    for item in found.captures.iter().filter(|c| c.index == *capture) {
                        let start = item.node.start_byte();
                        let line = ctx.lines.index(start);
                        hits.push(Hit {
                            line,
                            column: start - ctx.lines.starts[line],
                            start,
                            end: item.node.end_byte(),
                        });
                    }
                }
            }
            if cursor.did_exceed_match_limit() {
                return Err(format!("query match limit exceeded for {}", rule.id));
            }
        }
        Matcher::Regex(regex) => {
            // The prefilter runs on raw text, so files and lines without candidates
            // skip masking and the main regex entirely.
            let raw = &ctx.lines;
            let candidates: Option<Vec<usize>> = match (&rule.prefilter, rule.scope) {
                (Some(p), Scope::File) if !p.is_match(ctx.text) => Some(Vec::new()),
                (Some(p), Scope::Line) => Some(
                    (0..raw.count())
                        .filter(|&l| p.is_match(raw.line(l)))
                        .collect(),
                ),
                _ => None,
            };
            if candidates.as_ref().is_some_and(Vec::is_empty) {
                return Ok(hits);
            }
            let subject = ctx.subject(rule, cursor)?;
            let lines = Lines::new(&subject);
            let mut push = |line: usize, offset: usize, len: usize| {
                let base = ctx.lines.starts[line];
                let width = ctx.lines.line(line).len();
                hits.push(Hit {
                    line,
                    column: offset.min(width),
                    start: base + offset.min(width),
                    end: base + (offset + len).min(width),
                });
            };
            match rule.scope {
                Scope::File => {
                    for found in regex.find_iter(&subject) {
                        let found =
                            found.map_err(|e| format!("regex failed for {}: {e}", rule.id))?;
                        if rule
                            .match_length
                            .is_some_and(|length| !length.matches(found.as_str()))
                        {
                            continue;
                        }
                        let line = lines.index(found.start());
                        push(
                            line,
                            found.start() - lines.starts[line],
                            found.end() - found.start(),
                        );
                    }
                }
                Scope::Line => {
                    let all: Vec<usize> = (0..lines.count()).collect();
                    for &line in candidates.as_deref().unwrap_or(&all) {
                        for found in regex.find_iter(lines.line(line)) {
                            let found =
                                found.map_err(|e| format!("regex failed for {}: {e}", rule.id))?;
                            if rule
                                .match_length
                                .is_some_and(|length| !length.matches(found.as_str()))
                            {
                                continue;
                            }
                            push(line, found.start(), found.end() - found.start());
                        }
                    }
                }
            }
        }
    }
    if hits.is_empty() {
        return Ok(hits);
    }
    if let Some(skip) = &rule.skip_lines {
        let mut skipped = BTreeSet::new();
        if let Some(regex) = &skip.regex {
            let subject = ctx.subject(rule, cursor)?;
            let lines = Lines::new(&subject);
            for hit in &hits {
                if hit.line < lines.count()
                    && regex
                        .is_match(lines.line(hit.line))
                        .map_err(|e| e.to_string())?
                {
                    skipped.insert(hit.line + 1);
                }
            }
        }
        if let Some(query) = &skip.query {
            skipped.extend(query_lines(cursor, query, ctx)?);
        }
        hits.retain(|h| !skipped.contains(&(h.line + 1)));
    }
    // A node matched several ways (alternative patterns, repeated captures) is one finding.
    hits.sort_by_key(|h| (h.start, h.end));
    hits.dedup_by_key(|h| (h.start, h.end));
    if let Some(query) = &rule.skip_inside {
        let ranges = capture_ranges(cursor, query, ctx)?;
        hits.retain(|h| !ranges.iter().any(|r| r.contains(&h.start)));
    }
    if rule.limit == Some(Limit::FirstPerFile) {
        hits.sort_by_key(|h| h.start);
        hits.truncate(1);
    }
    Ok(hits)
}

/// Lint one file. `only` restricts the run to one rule (used by rule tests).
pub(crate) fn source(
    engine: &Engine,
    only: Option<&Rule>,
    parser: &mut Parser,
    cursor: &mut QueryCursor,
    file: &str,
    text: &str,
) -> std::result::Result<Vec<Finding>, Diagnostic> {
    let fail = |message| diagnostic(file, 1, 1, message);
    let started = Instant::now();
    let tree = parse(parser, text).map_err(fail)?;
    let slots = engine.rules.len();
    record(engine, slots, started);
    let known: BTreeSet<_> = engine
        .rule_ids()
        .chain(only.map(|r| r.id.as_str()))
        .collect();
    let mut directives = Directives::default();
    let wants_credo = engine.credo_directives
        && text
            .as_bytes()
            .windows(6)
            .any(|w| w.eq_ignore_ascii_case(b"credo:"));
    let mut nodes = if tree.root_node().has_error() || text.contains("fino:") || wants_credo {
        vec![tree.root_node()]
    } else {
        Vec::new()
    };
    while let Some(node) = nodes.pop() {
        let point = node.start_position();
        if node.is_error() || node.is_missing() {
            return Err(diagnostic(
                file,
                point.row + 1,
                point.column + 1,
                "Elixir syntax error or missing token".into(),
            ));
        }
        if node.kind() == "comment" {
            directives
                .add_comment(&text[node.byte_range()], point.row + 1, &known, wants_credo)
                .map_err(|message| diagnostic(file, point.row + 1, point.column + 1, message))?;
        }
        let mut walk = node.walk();
        nodes.extend(node.children(&mut walk));
    }
    if tree.root_node().has_error() {
        return Err(fail("Elixir parse was incomplete".into()));
    }
    let mut ctx = FileCtx {
        text,
        lines: Lines::new(text),
        tree,
        regions: None,
        masked: HashMap::new(),
    };
    let mut findings = Vec::new();
    let rules: Vec<(Option<usize>, &Rule)> = match only {
        Some(rule) => vec![(None, rule)],
        None => engine
            .rules
            .iter()
            .enumerate()
            .map(|(i, r)| (Some(i), r))
            .collect(),
    };
    let mut active = Vec::new();
    for (index, rule) in rules {
        let credo = rule.credo.as_deref();
        if (!rule.include.is_empty() && !rule.include.is_match(file))
            || path_excluded(&rule.exclude, Path::new(file))
            || directives.file_disabled(&rule.id, credo)
        {
            continue;
        }
        if let Some(cond) = &rule.if_file
            && !file_condition(cursor, cond, &ctx).map_err(fail)?
        {
            continue;
        }
        if let Some(cond) = &rule.unless_file
            && file_condition(cursor, cond, &ctx).map_err(fail)?
        {
            continue;
        }
        active.push((index, rule));
    }
    // One traversal per query group for every active query rule (fast loads only).
    let mut shared: HashMap<usize, Vec<Hit>> = HashMap::new();
    if !engine.groups.is_empty()
        && active
            .iter()
            .any(|(i, r)| i.is_some() && matches!(r.matcher, Matcher::Query { .. }))
    {
        let started = Instant::now();
        for (i, r) in &active {
            if let (Some(i), Matcher::Query { .. }) = (i, &r.matcher) {
                shared.insert(*i, Vec::new());
            }
        }
        for group in &engine.groups {
            if !group.pattern_rule.iter().any(|r| shared.contains_key(r)) {
                continue;
            }
            let query = &group.query;
            let mut matches = cursor.matches(query, ctx.tree.root_node(), text.as_bytes());
            while let Some(found) = matches.next() {
                let Some(hits) = shared.get_mut(&group.pattern_rule[found.pattern_index]) else {
                    continue;
                };
                if !predicates_hold(query, found) {
                    continue;
                }
                for item in found.captures.iter().filter(|c| c.index == group.capture) {
                    let start = item.node.start_byte();
                    let line = ctx.lines.index(start);
                    hits.push(Hit {
                        line,
                        column: start - ctx.lines.starts[line],
                        start,
                        end: item.node.end_byte(),
                    });
                }
            }
            if cursor.did_exceed_match_limit() {
                return Err(fail("query match limit exceeded".into()));
            }
        }
        record(engine, slots + 1, started);
    }
    for (index, rule) in active {
        let credo = rule.credo.as_deref();
        let started = Instant::now();
        let hits = rule_hits(
            rule,
            cursor,
            &mut ctx,
            index.and_then(|i| shared.remove(&i)),
        )
        .map_err(fail)?;
        if let Some(index) = index {
            record(engine, index, started);
        }
        for hit in hits {
            let line = hit.line + 1;
            if directives.line_disabled(&rule.id, credo, line) {
                continue;
            }
            findings.push(Finding {
                file: file.into(),
                line,
                column: hit.column + 1,
                rule: rule.id.clone(),
                message: rule.message.clone(),
                severity: rule.severity,
                start_byte: hit.start,
                end_byte: hit.end,
            });
            if findings.len() > MAX_FINDINGS {
                return Err(fail("per-file finding limit exceeded".into()));
            }
        }
    }
    Ok(findings)
}
