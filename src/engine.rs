//! Loads the project configuration and rule files and compiles them.
use crate::Result;
use crate::model::{Condition, Limit, ProjectConfig, RuleFile, RuleTest, Scope, Severity};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Language, Query};

pub(crate) enum Matcher {
    /// `query` is compiled for strict loads (validate/test); fast loads compile it in a group.
    Query {
        query: Option<Query>,
        capture: u32,
    },
    Regex(fancy_regex::Regex),
    PathOnly,
}

pub(crate) struct MaskLayer {
    pub nodes: Vec<crate::model::MaskNode>,
    pub query: Option<Query>,
    /// Replacement byte; `None` removes masked content.
    pub with: Option<u8>,
    pub fill: bool,
}

pub(crate) struct Cond {
    pub regex: Option<fancy_regex::Regex>,
    pub query: Option<Query>,
}

pub struct Rule {
    pub id: String,
    pub message: String,
    pub severity: Severity,
    pub file: PathBuf,
    pub tests: Vec<RuleTest>,
    pub(crate) credo: Option<String>,
    pub(crate) include: GlobSet,
    pub(crate) exclude: GlobSet,
    pub(crate) matcher: Matcher,
    pub(crate) query_text: Option<String>,
    pub(crate) prefilter: Option<regex::Regex>,
    pub(crate) scope: Scope,
    pub(crate) masks: Vec<MaskLayer>,
    pub(crate) skip_lines: Option<Cond>,
    pub(crate) skip_inside: Option<Query>,
    pub(crate) if_file: Option<Cond>,
    pub(crate) unless_file: Option<Cond>,
    pub(crate) limit: Option<Limit>,
}

/// Several query rules compiled as one query, so each file is traversed once per group.
pub(crate) struct Group {
    pub query: Query,
    pub pattern_rule: Vec<usize>,
    pub capture: u32,
}

pub struct Engine {
    pub rules: Vec<Rule>,
    pub(crate) groups: Vec<Group>,
    /// Cumulative nanoseconds: one slot per rule, then parse, then the combined query pass.
    pub(crate) timings: Vec<std::sync::atomic::AtomicU64>,
    pub config_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub(crate) include: GlobSet,
    pub(crate) exclude: GlobSet,
    pub(crate) credo_directives: bool,
}

/// Problems found while compiling one rule file. Warnings never block a run.
#[derive(Debug, Default, serde::Serialize)]
pub struct Problems {
    pub file: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn language() -> Language {
    tree_sitter_elixir::LANGUAGE.into()
}

pub(crate) fn globs(patterns: &[String]) -> std::result::Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid path glob {pattern:?}: {e}"))?,
        );
    }
    builder.build().map_err(|e| e.to_string())
}

/// Engine predicates on top of tree-sitter's text predicates. Each counts the named,
/// non-comment children of a capture: `(#count-eq? @args 2)`.
pub(crate) const COUNT_PREDICATES: [&str; 3] = ["count-eq?", "count-gt?", "count-lt?"];

fn compile_query(text: &str, finding: bool) -> std::result::Result<(Query, Option<u32>), String> {
    let query = Query::new(&language(), text).map_err(|e| format!("invalid query: {e}"))?;
    for index in 0..query.pattern_count() {
        if !query.property_settings(index).is_empty()
            || !query.property_predicates(index).is_empty()
        {
            return Err("query directives (#set!, #is?) are not supported".into());
        }
        for predicate in query.general_predicates(index) {
            let valid = COUNT_PREDICATES.contains(&predicate.operator.as_ref())
                && matches!(
                    predicate.args.as_ref(),
                    [tree_sitter::QueryPredicateArg::Capture(_), tree_sitter::QueryPredicateArg::String(n)]
                        if n.parse::<usize>().is_ok()
                );
            if !valid {
                return Err(format!(
                    "unsupported predicate #{}; use #eq?, #match?, #any-of?, their not- forms, \
                     or #count-eq?/#count-gt?/#count-lt? @capture N",
                    predicate.operator
                ));
            }
        }
    }
    let capture = query.capture_index_for_name("finding");
    if finding && capture.is_none() {
        return Err("query needs a @finding capture".into());
    }
    Ok((query, capture))
}

fn compile_regex(text: &str) -> std::result::Result<fancy_regex::Regex, String> {
    fancy_regex::Regex::new(text).map_err(|e| format!("invalid regex {text:?}: {e}"))
}

fn compile_cond(name: &str, cond: &Option<Condition>, errors: &mut Vec<String>) -> Option<Cond> {
    let cond = cond.as_ref()?;
    if cond.regex.is_none() && cond.query.is_none() {
        errors.push(format!("{name} needs regex or query"));
        return None;
    }
    let regex = cond.regex.as_deref().and_then(|r| {
        compile_regex(r)
            .map_err(|e| errors.push(format!("{name}: {e}")))
            .ok()
    });
    let query = cond.query.as_deref().and_then(|q| {
        compile_query(q, false)
            .map(|(q, _)| q)
            .map_err(|e| errors.push(format!("{name}: {e}")))
            .ok()
    });
    Some(Cond { regex, query })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Parse and compile one rule file, collecting every problem.
pub fn compile_rule(path: &Path) -> (Option<Rule>, Problems) {
    compile_rule_with(path, true)
}

/// `strict` compiles each rule's own query; otherwise query rules are compiled later in groups.
pub(crate) fn compile_rule_with(path: &Path, strict: bool) -> (Option<Rule>, Problems) {
    let mut problems = Problems {
        file: path.display().to_string(),
        ..Problems::default()
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            problems.errors.push(format!("cannot read: {e}"));
            return (None, problems);
        }
    };
    let spec: RuleFile = match toml::from_str(&text) {
        Ok(spec) => spec,
        Err(e) => {
            problems.errors.push(e.to_string().trim().to_string());
            return (None, problems);
        }
    };
    let rule = compile_spec(spec, path, &mut problems, strict);
    (
        if problems.errors.is_empty() {
            rule
        } else {
            None
        },
        problems,
    )
}

fn compile_spec(
    spec: RuleFile,
    path: &Path,
    problems: &mut Problems,
    strict: bool,
) -> Option<Rule> {
    let errors = &mut problems.errors;
    if !valid_id(&spec.id) {
        errors.push(format!(
            "id {:?} must be lowercase letters, digits and single dashes",
            spec.id
        ));
    }
    if path.file_stem().and_then(|s| s.to_str()) != Some(spec.id.as_str()) {
        errors.push(format!("file name must be {}.toml", spec.id));
    }
    if spec.message.trim().is_empty() {
        errors.push("message must not be empty".into());
    }
    let matchers = [spec.query.is_some(), spec.regex.is_some(), spec.path_only]
        .iter()
        .filter(|m| **m)
        .count();
    if matchers != 1 {
        errors.push("set exactly one of query, regex or path_only".into());
    }
    if spec.regex.is_none()
        && (spec.scope == Scope::Line || spec.mask.is_some() || spec.prefilter.is_some())
    {
        errors.push("scope, mask and prefilter only apply to regex rules".into());
    }
    let mut masks = Vec::new();
    for (n, mask) in spec.mask.iter().flat_map(|m| m.layers()).enumerate() {
        if mask.nodes.is_empty() && mask.query.is_none() {
            errors.push(format!("mask[{n}] needs nodes or query"));
        }
        if !(mask.with.is_empty()
            || (mask.with.len() == 1 && mask.with.is_ascii() && mask.with != "\n"))
        {
            errors.push(format!(
                "mask[{n}].with must be one ASCII character or \"\""
            ));
        }
        if mask.fill_blank_lines && mask.with.is_empty() {
            errors.push(format!(
                "mask[{n}].fill_blank_lines needs a non-empty `with`"
            ));
        }
        let query = mask.query.as_deref().and_then(|q| {
            compile_query(q, false)
                .map(|(q, _)| q)
                .map_err(|e| errors.push(format!("mask[{n}]: {e}")))
                .ok()
        });
        masks.push(MaskLayer {
            nodes: mask.nodes.clone(),
            query,
            with: mask.with.as_bytes().first().copied(),
            fill: mask.fill_blank_lines,
        });
    }
    let matcher = if let Some(text) = &spec.query {
        if strict {
            compile_query(text, true)
                .map(|(query, capture)| Matcher::Query {
                    query: Some(query),
                    capture: capture.unwrap_or_default(),
                })
                .map_err(|e| errors.push(e))
                .ok()
        } else {
            Some(Matcher::Query {
                query: None,
                capture: 0,
            })
        }
    } else if let Some(text) = &spec.regex {
        compile_regex(text)
            .map(Matcher::Regex)
            .map_err(|e| errors.push(e))
            .ok()
    } else {
        Some(Matcher::PathOnly)
    };
    let prefilter = spec.prefilter.as_deref().and_then(|p| {
        regex::Regex::new(p)
            .map_err(|e| errors.push(format!("prefilter must be a plain regex: {e}")))
            .ok()
    });
    let include = globs(&spec.include).map_err(|e| errors.push(e)).ok();
    let exclude = globs(&spec.exclude).map_err(|e| errors.push(e)).ok();
    let skip_lines = compile_cond("skip_lines", &spec.skip_lines, errors);
    let skip_inside = spec.skip_inside.as_deref().and_then(|q| {
        compile_query(q, false)
            .map(|(q, _)| q)
            .map_err(|e| errors.push(format!("skip_inside: {e}")))
            .ok()
    });
    let if_file = compile_cond("if_file", &spec.if_file, errors);
    let unless_file = compile_cond("unless_file", &spec.unless_file, errors);
    for (n, test) in spec.tests.iter().enumerate() {
        if test.expect.contains(&0) {
            errors.push(format!(
                "tests[{n}] ({}): expect uses one-based lines",
                test.name
            ));
        }
        if crate::snapshot::valid_relative_path(&test.path).not() {
            errors.push(format!("tests[{n}] ({}): path must be relative", test.name));
        }
    }
    if spec.tests.is_empty() {
        problems
            .warnings
            .push("no tests; add [[tests]] with a finding and a clean example".into());
    } else {
        if spec.tests.iter().all(|t| t.expect.is_empty()) {
            problems.warnings.push("no test expects a finding".into());
        }
        if spec.tests.iter().all(|t| !t.expect.is_empty()) {
            problems.warnings.push("no clean (expect = []) test".into());
        }
    }
    if !problems.errors.is_empty() {
        return None;
    }
    Some(Rule {
        id: spec.id,
        message: spec.message,
        severity: spec.severity,
        file: path.to_path_buf(),
        tests: spec.tests,
        credo: spec.compat.and_then(|c| c.credo),
        include: include?,
        exclude: exclude?,
        matcher: matcher?,
        query_text: spec.query,
        prefilter,
        scope: spec.scope,
        masks,
        skip_lines,
        skip_inside,
        if_file,
        unless_file,
        limit: spec.limit,
    })
}

use std::ops::Not;

/// Every `*.toml` file in the rules directory, sorted.
pub fn rule_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("cannot read rules directory {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

pub fn load_config(path: &Path) -> Result<ProjectConfig> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text)
        .map_err(|e| format!("invalid {}: {}", path.display(), e.to_string().trim()))
}

impl Engine {
    /// Load configuration and compile rules. `only` restricts loading to these rule files.
    /// Strict: every rule's own query is compiled (validate, test).
    pub fn load(
        config_path: &Path,
        only: Option<&[PathBuf]>,
    ) -> std::result::Result<(Self, Vec<Problems>), String> {
        Self::load_with(config_path, only, true)
    }

    /// For linting: query rules are compiled only in parallel groups.
    pub fn load_fast(config_path: &Path) -> std::result::Result<(Self, Vec<Problems>), String> {
        Self::load_with(config_path, None, false)
    }

    fn load_with(
        config_path: &Path,
        only: Option<&[PathBuf]>,
        strict: bool,
    ) -> std::result::Result<(Self, Vec<Problems>), String> {
        let config = load_config(config_path)?;
        let dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let files = match only {
            Some(files) => files.to_vec(),
            None => rule_files(&dir.join(&config.rules_dir))?,
        };
        let mut rules = Vec::new();
        let mut problems = Vec::new();
        let mut ids = BTreeSet::new();
        use rayon::prelude::*;
        let compiled: Vec<_> = files
            .par_iter()
            .map(|file| compile_rule_with(file, strict))
            .collect();
        for (rule, mut found) in compiled {
            if let Some(rule) = rule {
                if ids.insert(rule.id.clone()) {
                    rules.push(rule);
                } else {
                    found.errors.push(format!("duplicate rule id {}", rule.id));
                }
            }
            problems.push(found);
        }
        if problems.iter().all(|p| p.errors.is_empty()) && config.include.is_empty() {
            return Err("config include must not be empty".into());
        }
        let groups = if strict {
            Vec::new()
        } else {
            group_queries(&rules, &mut problems)
        };
        let timings = (0..rules.len() + 2)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect();
        let engine = Self {
            rules,
            groups,
            timings,
            config_path: config_path.to_path_buf(),
            snapshot_path: dir.join(&config.snapshot_file),
            include: globs(&config.include)?,
            exclude: globs(&config.exclude)?,
            credo_directives: config.compat.credo_directives,
        };
        Ok((engine, problems))
    }

    /// Cumulative time per rule, plus parsing and the shared query pass, slowest first.
    pub fn timings(&self) -> Vec<(String, std::time::Duration)> {
        use std::sync::atomic::Ordering::Relaxed;
        let names = self
            .rules
            .iter()
            .map(|r| r.id.clone())
            .chain(["(parse)".into(), "(query pass)".into()]);
        let mut rows: Vec<_> = names
            .zip(&self.timings)
            .map(|(name, nanos)| (name, std::time::Duration::from_nanos(nanos.load(Relaxed))))
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.1));
        rows
    }

    pub fn rule_ids(&self) -> impl Iterator<Item = &str> {
        self.rules.iter().map(|r| r.id.as_str())
    }
}

/// Compile query rules in a few groups, in parallel. A group that fails is recompiled
/// rule by rule so errors point at the right file.
fn group_queries(rules: &[Rule], problems: &mut [Problems]) -> Vec<Group> {
    use rayon::prelude::*;
    let query_rules: Vec<usize> = (0..rules.len())
        .filter(|&i| rules[i].query_text.is_some())
        .collect();
    if query_rules.is_empty() {
        return Vec::new();
    }
    let count = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, 8)
        .min(query_rules.len());
    // Balance groups by query length, a rough proxy for compile cost.
    let mut bins: Vec<(usize, Vec<usize>)> = vec![(0, Vec::new()); count];
    let mut by_size = query_rules.clone();
    by_size
        .sort_by_key(|&i| std::cmp::Reverse(rules[i].query_text.as_ref().map_or(0, String::len)));
    for i in by_size {
        let bin = bins.iter_mut().min_by_key(|b| b.0).expect("non-empty");
        bin.0 += rules[i].query_text.as_ref().map_or(0, String::len);
        bin.1.push(i);
    }
    let compiled: Vec<std::result::Result<Group, Vec<usize>>> = bins
        .into_par_iter()
        .map(|(_, members)| compile_group(rules, &members).ok_or(members))
        .collect();
    let mut groups = Vec::new();
    for result in compiled {
        match result {
            Ok(group) => groups.push(group),
            Err(members) => {
                for i in members {
                    let text = rules[i].query_text.as_deref().unwrap_or_default();
                    let error = compile_query(text, true)
                        .err()
                        .unwrap_or_else(|| "query could not be grouped".into());
                    if let Some(found) = problems
                        .iter_mut()
                        .find(|p| Path::new(&p.file) == rules[i].file)
                    {
                        found.errors.push(error);
                    }
                }
            }
        }
    }
    groups
}

fn compile_group(rules: &[Rule], members: &[usize]) -> Option<Group> {
    let mut text = String::new();
    let mut spans = Vec::new();
    for &i in members {
        let start = text.len();
        text.push_str(rules[i].query_text.as_deref()?);
        text.push('\n');
        spans.push((start, text.len(), i));
    }
    let (query, capture) = compile_query(&text, false).ok()?;
    let capture = capture?;
    let mut pattern_rule = Vec::with_capacity(query.pattern_count());
    let mut has_finding = vec![false; rules.len()];
    for pattern in 0..query.pattern_count() {
        let offset = query.start_byte_for_pattern(pattern);
        let rule = spans.iter().find(|(s, e, _)| (*s..*e).contains(&offset))?.2;
        if query.capture_quantifiers(pattern)[capture as usize]
            != tree_sitter::CaptureQuantifier::Zero
        {
            has_finding[rule] = true;
        }
        pattern_rule.push(rule);
    }
    // Every rule needs @finding; otherwise fall back to per-rule errors.
    members.iter().all(|&i| has_finding[i]).then_some(Group {
        query,
        pattern_rule,
        capture,
    })
}
