use super::*;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const DEBUG: &str = r#"
id = "debug"
message = "Remove debug"
query = '''((dot left: (alias) @_module right: (identifier) @finding)
  (#eq? @_module "IO") (#eq? @finding "inspect"))'''
"#;

fn fixture() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-fixtures")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&path).unwrap();
    path
}

/// A project root with `.fino/config.toml` and one file per rule.
fn project(config: &str, rules: &[&str]) -> (PathBuf, Engine) {
    let root = fixture();
    let dir = root.join(".fino/rules");
    fs::create_dir_all(&dir).unwrap();
    fs::write(root.join(".fino/config.toml"), config).unwrap();
    for rule in rules {
        let spec: model::RuleFile = toml::from_str(rule).unwrap();
        fs::write(dir.join(format!("{}.toml", spec.id)), rule).unwrap();
    }
    let (engine, problems) = Engine::load(&root.join(".fino/config.toml"), None).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    (root, engine)
}

fn engine(rules: &[&str]) -> Engine {
    project("", rules).1
}

fn lint(engine: &Engine, path: &str, text: &str) -> std::result::Result<Vec<Finding>, Diagnostic> {
    let (mut parser, mut cursor) = scan::worker();
    scan::source(engine, None, &mut parser, &mut cursor, path, text)
}

fn lines(engine: &Engine, path: &str, text: &str) -> Vec<usize> {
    lint(engine, path, text)
        .unwrap()
        .iter()
        .map(|f| f.line)
        .collect()
}

fn findings(text: &str) -> Vec<Finding> {
    lint(&engine(&[DEBUG]), "lib/sample.ex", text).unwrap()
}

#[test]
fn auxiliary_queries_fail_closed_when_match_limit_is_exceeded() {
    let query = "(list (identifier) @a (identifier) @b)";
    let text = format!("x = [{}]\n", vec!["value"; 50].join(", "));
    for condition in [
        format!("mask = {{ query = '{query}' }}"),
        format!("skip_lines = {{ query = '{query}' }}"),
        format!("skip_inside = '{query}'"),
        format!("if_file = {{ query = '{query}' }}"),
        format!("unless_file = {{ query = '{query}' }}"),
    ] {
        let engine = engine(&[&format!(
            "id = 'limit'\nmessage = 'limit'\nregex = 'x'\n{condition}\n"
        )]);
        let (mut parser, mut cursor) = scan::worker();
        cursor.set_match_limit(1);
        let error = scan::source(&engine, None, &mut parser, &mut cursor, "example.ex", &text)
            .expect_err(&condition);
        assert!(
            error.message.contains("query match limit exceeded"),
            "{error:?}"
        );
    }
}

#[test]
fn query_finds_nested_multiline_piped_bare_and_captured_calls() {
    let rows = findings(
        "IO.inspect(IO.inspect(:x))\nIO\n .inspect(:x)\n:x |> IO.inspect()\n&IO.inspect/1\nIO.inspect\n",
    );
    let mut found: Vec<_> = rows.iter().map(|r| r.line).collect();
    found.sort();
    assert_eq!(found, [1, 1, 3, 4, 5, 6]);
}

#[test]
fn query_ignores_comments_strings_sigils_and_unresolved_aliases() {
    let text = "# IO.inspect(:x)\n\"IO.inspect(:x)\"\n~S|IO.inspect(:x)|\nalias IO, as: Console\nConsole.inspect(:x)\nimport IO\ninspect(:x)\n";
    assert!(findings(text).is_empty());
    assert_eq!(findings(r##""#{IO.inspect(:x)}""##).len(), 1);
}

#[test]
fn query_locations_use_one_based_byte_columns() {
    let text = "value = \"é\"; IO.inspect(:x)\r\n";
    let rows = findings(text);
    assert_eq!(
        (rows[0].line, rows[0].column),
        (1, text.find("inspect").unwrap() + 1)
    );
}

#[test]
fn nested_case_preserves_valid_parse_paths_and_query_findings() {
    let source = include_str!("fixtures/nested_case.ex");
    let with_finding = source.replace(":ok\nend\nend)", "IO.inspect(:ok)\nend\nend)");
    assert_ne!(source, with_finding);
    for _ in 0..32 {
        assert!(findings(source).is_empty());
        assert_eq!(findings(&with_finding).len(), 1);
    }
}

#[test]
fn native_directives_are_real_comments_and_can_target_one_rule() {
    let rows = findings(
        "# fino:disable-next-line debug -- reviewed\nIO.inspect(:x)\nIO.inspect(:y) # fino:disable-line debug\nIO.inspect(:z)\n",
    );
    assert_eq!(rows.iter().map(|r| r.line).collect::<Vec<_>>(), [4]);
    assert!(findings("# fino:disable-file all\nIO.inspect(:x)\n").is_empty());
    assert_eq!(
        findings("\"# fino:disable-file all\"\nIO.inspect(:x)\n").len(),
        1
    );
}

#[test]
fn credo_directives_follow_credo_semantics_when_enabled() {
    let rule = format!("{DEBUG}\n[compat]\ncredo = \"Credo.Check.Warning.IoInspect\"\n");
    let on = project("[compat]\ncredo_directives = true\n", &[&rule]).1;
    let off = engine(&[&rule]);
    let text = "# credo:disable-for-next-line Credo.Check.Warning.IoInspect\nIO.inspect(1)\n\
                IO.inspect(2) # credo:disable-for-previous-line\nIO.inspect(3)\n\
                # credo:disable-for-lines:2 /ioinspect/\nIO.inspect(4)\nIO.inspect(5)\n\
                # credo:disable-for-next-line Credo.Check.Other\nIO.inspect(6)\n\
                # credo:disable-for-next-line Credo.Check.Warning.IoInspect -- reason\nIO.inspect(7)\n";
    assert_eq!(lines(&on, "lib/a.ex", text), [3, 4, 9, 11]);
    assert_eq!(lines(&off, "lib/a.ex", text).len(), 7);
    assert!(
        lines(
            &on,
            "lib/a.ex",
            "# credo:disable-for-this-file\nIO.inspect(1)\n"
        )
        .is_empty()
    );
    // Rules without a Credo name are unaffected by Credo comments.
    let native = project("[compat]\ncredo_directives = true\n", &[DEBUG]).1;
    assert_eq!(
        lines(
            &native,
            "lib/a.ex",
            "# credo:disable-for-this-file\nIO.inspect(1)\n"
        ),
        [2]
    );
}

#[test]
fn malformed_source_and_unknown_directives_fail_closed() {
    let engine = engine(&[DEBUG]);
    for text in [
        "defmodule Missing do\n",
        "# fino:disable-file typo\nIO.inspect(:x)",
        "# fino:disabel-file all",
    ] {
        assert!(lint(&engine, "bad.ex", text).is_err());
    }
}

#[test]
fn line_regex_with_masks_ignores_strings_and_comments() {
    let rule = r#"
id = "comma"
message = "Space missing after comma."
regex = '(?<!\W\?),\S'
scope = "line"
mask = { nodes = ["string", "sigil", "charlist", "comment"] }
"#;
    let engine = engine(&[rule]);
    let text = "f(a,b)\n\"a,b\"\n~w(a,b)\n'a,b'\n# a,b\n[?,, ?a]\nx = \"#{f(a,b)}\"\n";
    assert_eq!(lines(&engine, "lib/a.ex", text), [1]);
}

#[test]
fn removal_masks_and_skip_lines_express_line_length_limits() {
    let rule = r#"
id = "long"
message = "Line too long."
regex = '^.{21,}$'
scope = "line"
mask = { nodes = ["string"], with = "" }
skip_lines = { query = '((call target: (identifier) @d) (#any-of? @d "def" "defp"))' }
"#;
    let engine = engine(&[rule]);
    let text = "x = \"a very very very long string\"\nyyyyyyyyyyyyyyyyyyyyyyyyy = 1\ndef fffffffffffffffffffffff, do: 1\n";
    assert_eq!(lines(&engine, "lib/a.ex", text), [2]);
}

#[test]
fn look_around_regex_finds_consecutive_blank_lines() {
    let rule = "id = \"blank\"\nmessage = \"Too many blank lines.\"\nregex = '(?m)(?<=^\\n)^$'\n";
    let engine = engine(&[rule]);
    assert_eq!(lines(&engine, "lib/a.ex", "a\n\n\n\nb\n\nc\n\n"), [3, 4, 9]);
}

#[test]
fn file_conditions_limits_and_path_only_rules() {
    let marker = r#"
id = "marker"
message = "This example plugin requires a review marker."
query = '(call target: (identifier) @_u (#eq? @_u "use") (arguments (alias) @finding (#eq? @finding "Example.Plugin")))'
unless_file = { regex = '#\s*policy:\s*reviewed\b' }
limit = "first-per-file"
"#;
    let location = r#"
id = "location"
message = "This example policy disallows legacy paths."
path_only = true
include = ["legacy/**/*.ex"]
exclude = ["legacy/compat"]
"#;
    let engine = engine(&[marker, location]);
    assert_eq!(
        lines(
            &engine,
            "lib/w.ex",
            "use Example.Plugin\nuse Example.Plugin\n"
        ),
        [1]
    );
    assert!(
        lines(
            &engine,
            "lib/w.ex",
            "use Example.Plugin\n# policy: reviewed\n"
        )
        .is_empty()
    );
    assert_eq!(lines(&engine, "legacy/a.ex", ":ok\n"), [1]);
    assert!(lines(&engine, "legacy/compat/a.ex", ":ok\n").is_empty());
}

#[test]
fn warnings_are_reported_but_do_not_fail() {
    let rule = format!("{DEBUG}\nseverity = \"warning\"\n");
    let (root, engine) = project("", &[&rule]);
    fs::write(root.join("a.ex"), "IO.inspect(1)\n").unwrap();
    let report = engine.scan(&root, None, 2, &Snapshot::default()).unwrap();
    assert_eq!((report.findings.len(), report.exit_code()), (1, 0));
}

#[test]
fn invalid_rules_report_every_problem() {
    let root = fixture();
    let path = root.join("wrong-name.toml");
    fs::write(
        &path,
        "id = \"Bad_Id\"\nmessage = \"\"\nquery = '(identifier) @other'\nregex = '['\n",
    )
    .unwrap();
    let (rule, problems) = compile_rule(&path);
    assert!(rule.is_none());
    let text = problems.errors.join("\n");
    for expected in ["id", "file name", "message", "exactly one"] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    fs::write(&path, "id = \"wrong-name\"\nmessage = \"m\"\nquery = '((identifier) @finding (#custom? @finding))'\n").unwrap();
    assert!(compile_rule(&path).0.is_none());
    fs::write(
        &path,
        "id = \"wrong-name\"\nmessage = \"m\"\nregex = 'x'\nunknown = 1\n",
    )
    .unwrap();
    assert!(compile_rule(&path).0.is_none());
}

#[test]
fn rule_tests_report_expected_and_actual_lines() {
    let rule = format!(
        "{DEBUG}\n[[tests]]\nname = \"flags\"\ncode = \"IO.inspect(1)\"\nexpect = [1]\n\n\
         [[tests]]\nname = \"wrong\"\ncode = \"IO.inspect(1)\"\nexpect = []\n\n\
         [[tests]]\nname = \"broken\"\ncode = \"defmodule X do\"\nexpect = []\n"
    );
    let outcomes = run_tests(&engine(&[&rule]));
    let summary: Vec<_> = outcomes
        .iter()
        .map(|o| (o.name.as_str(), o.passed, o.actual.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            ("flags", true, vec![1]),
            ("wrong", false, vec![1]),
            ("broken", false, vec![])
        ]
    );
    assert!(outcomes[2].error.is_some());
}

#[test]
fn parallel_output_matches_serial_and_ignores_are_respected() {
    let (root, engine) = project("", &[DEBUG]);
    fs::create_dir(root.join("lib")).unwrap();
    fs::write(root.join("lib/a.ex"), "IO.inspect(:a)\n").unwrap();
    fs::write(root.join("lib/b.ex"), "IO.inspect(:b)\n").unwrap();
    fs::write(root.join("lib/ignored.ex"), "IO.inspect(:ignore)\n").unwrap();
    fs::write(root.join(".gitignore"), "lib/ignored.ex\n").unwrap();
    let serial = engine.scan(&root, None, 1, &Snapshot::default()).unwrap();
    let parallel = engine.scan(&root, None, 4, &Snapshot::default()).unwrap();
    assert_eq!(serial.files, 2);
    assert_eq!(serial.findings, parallel.findings);
    assert_eq!(serial.exit_code(), 1);
    let explicit = engine
        .scan(
            &root,
            Some(&["lib/ignored.ex".into()]),
            2,
            &Snapshot::default(),
        )
        .unwrap();
    assert_eq!(explicit.findings.len(), 1);
}

#[test]
fn manifests_reject_parent_traversal() {
    let (root, engine) = project("", &[DEBUG]);
    for path in ["../outside.ex", "/outside.ex"] {
        assert!(
            engine
                .scan(&root, Some(&[path.into()]), 1, &Snapshot::default())
                .is_err()
        );
    }
}

#[test]
fn snapshots_survive_line_moves_and_subsets_but_not_replacements() {
    let (root, engine) = project("", &[DEBUG]);
    fs::write(root.join("a.ex"), "IO.inspect(:a)\nIO.inspect(:a)\n").unwrap();
    fs::write(root.join("b.ex"), "IO.inspect(:b)\n").unwrap();
    let initial = engine.scan(&root, None, 1, &Snapshot::default()).unwrap();
    let policy = Snapshot(initial.current);
    let path = root.join("snapshot.jsonl");
    policy.save(&path).unwrap();
    assert_eq!(Snapshot::load(&path).unwrap().0, policy.0);
    fs::write(root.join("a.ex"), "\n\n  IO.inspect(:a)\nIO.inspect(:a)\n").unwrap();
    let files = ["a.ex".into(), "README.md".into(), "ignored.ts".into()];
    let moved = engine.scan(&root, Some(&files), 1, &policy).unwrap();
    assert!(moved.passed);
    assert!(moved.stale.is_empty());
    assert_eq!(moved.files, 1);
    fs::write(root.join("a.ex"), "IO.inspect(:new)\n").unwrap();
    let changed = engine.scan(&root, Some(&files), 1, &policy).unwrap();
    assert!(!changed.passed);
    assert_eq!(changed.new.len(), 1);
    assert_eq!(changed.stale.len(), 2);
    assert!(changed.stale.iter().all(|e| e.file == "a.ex"));
}

#[test]
fn snapshot_stale_scope_and_errors_are_explicit() {
    let (root, engine) = project("", &[DEBUG]);
    fs::write(root.join("a.ex"), "IO.inspect(:a)\n").unwrap();
    let report = engine.scan(&root, None, 1, &Snapshot::default()).unwrap();
    let policy = Snapshot(report.current);
    fs::write(root.join("a.ex"), ":ok\n").unwrap();
    let fixed = engine.scan(&root, None, 1, &policy).unwrap();
    assert!(fixed.passed);
    assert_eq!(fixed.stale.len(), 1);
    let skipped = engine
        .scan(&root, Some(&["README.md".into()]), 1, &policy)
        .unwrap();
    assert!(skipped.passed);
    assert!(skipped.stale.is_empty());
    assert!(
        engine
            .scan(&root, Some(&["missing.ex".into()]), 1, &policy)
            .is_err()
    );
    fs::write(root.join("a.ex"), "defmodule Broken do\n").unwrap();
    assert_eq!(engine.scan(&root, None, 1, &policy).unwrap().exit_code(), 2);
}

#[test]
fn snapshot_updates_preserve_ignored_files_and_reject_stale_writers() {
    let (root, engine) = project("", &[DEBUG]);
    fs::write(root.join("a.ex"), "IO.inspect(:a)\n").unwrap();
    fs::write(root.join("b.ex"), ":ok\n").unwrap();
    let initial = engine.scan(&root, None, 1, &Snapshot::default()).unwrap();
    let policy = Snapshot(initial.current);
    let path = root.join("snapshot.jsonl");
    policy.save(&path).unwrap();
    fs::write(root.join(".gitignore"), "a.ex\n").unwrap();
    assert!(
        engine
            .scan(&root, None, 1, &policy)
            .unwrap()
            .stale
            .is_empty()
    );
    let empty = std::collections::BTreeSet::new();
    policy
        .write_update(
            &path,
            &policy.0.iter().cloned().collect::<Vec<_>>(),
            &empty,
            false,
        )
        .unwrap();
    assert!(policy.write_update(&path, &[], &policy.0, true).is_err());
    assert!(Snapshot::load(&path).unwrap().0.is_empty());
}

#[test]
fn directory_exclusions_apply_to_explicit_manifests_too() {
    let (root, engine) = project("exclude = [\"lib/legacy\"]\n", &[DEBUG]);
    fs::create_dir_all(root.join("lib/legacy")).unwrap();
    fs::write(root.join("lib/keep.ex"), "IO.inspect(:x)\n").unwrap();
    fs::write(root.join("lib/legacy/old.ex"), "IO.inspect(:x)\n").unwrap();
    let manifest = ["lib/keep.ex".into(), "lib/legacy/old.ex".into()];
    let full = engine.scan(&root, None, 2, &Snapshot::default()).unwrap();
    let subset = engine
        .scan(&root, Some(&manifest), 2, &Snapshot::default())
        .unwrap();
    assert_eq!(full.files, 1);
    assert_eq!(full.findings, subset.findings);
}

#[test]
fn broken_gitignore_is_not_a_successful_policy_scan() {
    let (root, engine) = project("", &[DEBUG]);
    fs::write(root.join("sample.ex"), "IO.inspect(:x)\n").unwrap();
    fs::write(root.join(".gitignore"), "[z-a]\n").unwrap();
    assert!(engine.scan(&root, None, 1, &Snapshot::default()).is_err());
}

#[test]
fn layered_masks_select_by_query_and_fill_blank_lines() {
    let rule = r#"
id = "blank"
message = "Too many blank lines."
regex = '(?m)(?<=^\n)^$'
mask = [
  { nodes = ["sigil"], with = "=" },
  { nodes = ["string"], query = '((sigil (sigil_name) @_n) @s (#eq? @_n "s"))', with = "=", fill_blank_lines = true },
]
"#;
    let engine = engine(&[rule]);
    let text = "x = \"\"\"\na\n\n\nb\n\"\"\"\ny = ~w(\n\n\n)\nz = ~s(\n\n\n)\n";
    assert_eq!(lines(&engine, "lib/a.ex", text), [9]);
}

#[test]
fn count_predicates_ignore_comments() {
    let rule = r#"
id = "only-try"
message = "Body is only try."
query = '''
((call target: (identifier) @_def (do_block (call target: (identifier) @finding)) @_body)
 (#eq? @_def "def") (#eq? @finding "try") (#count-eq? @_body 1))
'''
"#;
    let engine = engine(&[rule]);
    let text = "def a do\n  # note\n  try do\n    x\n  rescue\n    _ -> y\n  end\nend\ndef b do\n  z\n  try do\n    x\n  rescue\n    _ -> y\n  end\nend\n";
    assert_eq!(lines(&engine, "lib/a.ex", text), [3]);
    let root = fixture();
    let path = root.join("bad-count.toml");
    fs::write(&path, "id = \"bad-count\"\nmessage = \"m\"\nquery = '((identifier) @finding (#count-eq? @finding x))'\n").unwrap();
    assert!(compile_rule(&path).0.is_none());
}

#[test]
fn fast_grouped_loading_matches_strict_loading() {
    let marker = "id = \"marker\"\nmessage = \"m\"\nquery = '((identifier) @finding (#eq? @finding \"marker\"))'\n";
    let (root, strict) = project("", &[DEBUG, marker]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    assert!(!fast.groups.is_empty());
    fs::write(root.join("a.ex"), "IO.inspect(marker)\nmarker\n").unwrap();
    let a = strict.scan(&root, None, 2, &Snapshot::default()).unwrap();
    let b = fast.scan(&root, None, 2, &Snapshot::default()).unwrap();
    assert_eq!(a.findings, b.findings);
    assert_eq!(a.findings.len(), 3);
    // A broken query is still reported against its own file.
    let bad = root.join(".fino/rules/broken.toml");
    fs::write(
        &bad,
        "id = \"broken\"\nmessage = \"m\"\nquery = '(not_a_node) @finding'\n",
    )
    .unwrap();
    for workers in [1, 2, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap();
        let (engine, problems) = pool
            .install(|| Engine::load_fast(&root.join(".fino/config.toml")))
            .unwrap();
        let errors: Vec<_> = problems
            .iter()
            .filter(|p| !p.errors.is_empty())
            .map(|p| p.file.clone())
            .collect();
        assert_eq!(errors.len(), 1, "{workers} workers: {problems:?}");
        assert!(errors[0].ends_with("broken.toml"));
        let grouped: std::collections::BTreeSet<_> = engine
            .groups
            .iter()
            .flat_map(|g| g.pattern_rule.iter().map(|&i| engine.rules[i].id.as_str()))
            .collect();
        assert_eq!(grouped, ["debug", "marker"].into_iter().collect());
    }
}

#[test]
fn regex_match_length_measures_only_the_match_in_the_selected_unicode_unit() {
    let text = "# x\n# é\n# e\u{301}\n# 👩‍💻\n";
    for (unit, expected) in [
        (Some("bytes"), vec![1]),
        (Some("scalars"), vec![1, 2]),
        (Some("graphemes"), vec![1, 2, 3, 4]),
        (None, vec![1, 2]),
    ] {
        for scope in ["line", "file"] {
            let unit = unit
                .map(|unit| format!(", unit = '{unit}'"))
                .unwrap_or_default();
            let rule = format!(
                "id = 'measured'\nmessage = 'Selected length.'\nregex = '(?m)(?<=# )[^\\n]*'\nscope = '{scope}'\nmatch_length = {{ min = 1, max = 1{unit} }}\n"
            );
            let (root, strict) = project("", &[&rule]);
            let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
            assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
            let rows = lint(&strict, "a.ex", text).unwrap();
            assert_eq!(
                rows.iter().map(|row| row.line).collect::<Vec<_>>(),
                expected,
                "{scope}: {unit}"
            );
            assert!(rows.iter().all(|row| row.column == 3));
            assert_eq!(rows, lint(&fast, "a.ex", text).unwrap());
        }
    }
}

#[test]
fn regex_match_length_has_inclusive_ascii_bounds_and_a_safe_scalar_prefilter() {
    let text = format!(
        "#{}\n#{}\n#{}\n#{}\n#{}\n",
        "x".repeat(119),
        "x".repeat(120),
        "x".repeat(121),
        "e\u{301}".repeat(60),
        "👩‍💻".repeat(40)
    );
    for (upper, expected) in [("", vec![2, 3]), (", max = 121", vec![2])] {
        let rule = format!(
            "id = 'boundary'\nmessage = 'Long text.'\nregex = '^.*$'\nscope = 'line'\nprefilter = '.{{121}}'\nmatch_length = {{ min = 121{upper}, unit = 'graphemes' }}\n"
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert_eq!(lines(engine, "a.ex", &text), expected);
        }
    }
}

#[test]
fn regex_match_length_runs_after_masking_and_keeps_existing_finding_coordinates() {
    for scope in ["line", "file"] {
        for (mask, bounds, expected_end) in [
            ("", "min = 4, max = 4", None),
            ("mask = { nodes = ['string'] }", "min = 4, max = 4", Some(8)),
            (
                "mask = { nodes = ['string'], with = '' }",
                "min = 2, max = 2",
                Some(6),
            ),
        ] {
            let rule = format!(
                "id = 'masked-length'\nmessage = 'Masked match.'\nregex = '\"[^\"]*\"'\nscope = '{scope}'\n{mask}\nmatch_length = {{ {bounds}, unit = 'graphemes' }}\n"
            );
            let (root, strict) = project("", &[&rule]);
            let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
            for engine in [&strict, &fast] {
                let text = if mask.contains("with") {
                    "x = \"payload\"\n"
                } else {
                    "x = \"é\"\n"
                };
                let rows = lint(engine, "a.ex", text).unwrap();
                if let Some(end) = expected_end {
                    assert_eq!(rows.len(), 1);
                    assert_eq!(
                        (
                            rows[0].line,
                            rows[0].column,
                            rows[0].start_byte,
                            rows[0].end_byte
                        ),
                        (1, 5, 4, end)
                    );
                } else {
                    assert!(rows.is_empty());
                }
            }
        }
    }
}

#[test]
fn regex_match_length_in_file_scope_includes_matched_newlines_not_line_newlines() {
    let text = "# a\n# b\n";
    for (scope, expected) in [("file", vec![1]), ("line", vec![])] {
        let rule = format!(
            "id = 'whole-match'\nmessage = 'Whole match.'\nregex = '(?s)^.*$'\nscope = '{scope}'\nmatch_length = {{ min = 8, max = 8, unit = 'bytes' }}\n"
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert_eq!(lines(engine, "a.ex", text), expected);
        }
    }
}

#[test]
fn regex_match_length_validation_fails_closed_in_both_loading_modes() {
    for (matcher, bounds, expected) in [
        (
            "query = '(identifier) @finding'",
            "min = 1",
            "match_length only applies to regex rules",
        ),
        (
            "path_only = true",
            "min = 1",
            "match_length only applies to regex rules",
        ),
        (
            "regex = 'x'",
            "",
            "match_length: text length requires a minimum or maximum",
        ),
        (
            "regex = 'x'",
            "min = 3, max = 2",
            "match_length: text length minimum must not exceed maximum",
        ),
        ("regex = 'x'", "min = -1", "invalid value"),
        ("regex = 'x'", "unit = 'cells', min = 1", "unknown variant"),
    ] {
        let (root, _) = project("", &[]);
        fs::write(root.join(".fino/rules/invalid.toml"), format!("id = 'invalid'\nmessage = 'Invalid length.'\n{matcher}\nmatch_length = {{ {bounds} }}\n")).unwrap();
        for strict in [true, false] {
            let (_, problems) = if strict {
                Engine::load(&root.join(".fino/config.toml"), None)
            } else {
                Engine::load_fast(&root.join(".fino/config.toml"))
            }
            .unwrap();
            assert!(
                problems
                    .iter()
                    .flat_map(|p| &p.errors)
                    .any(|error| error.contains(expected)),
                "{problems:?}"
            );
        }
    }
    let engine = engine(&[
        "id = 'syntax'\nmessage = 'Still invalid.'\nregex = 'never'\nmatch_length = { min = 100 }\n",
    ]);
    assert!(lint(&engine, "broken.ex", "defmodule (\n").is_err());
}
