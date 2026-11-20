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
fn elixir_patterns_normalize_pipes_literals_and_repeated_captures_in_both_modes() {
    let rule = include_str!("../examples/patterns/.fino/rules/identity-map.toml");
    let (root, strict) = project("", &[rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = r##"Enum.map(items, fn x -> x end)
items |> Enum.map(fn x -> (x) end)
Enum."m\x61p"(items, fn x -> x end)
Enum.map(items, fn x -> other end)
Enum.map(items, fn x -> callback.(x) end)
Enum.map(items, fn x -> "#{x}" end)
items |> Enum.map(fn x -> transform(x) end)
Enum.map(items, fn x -> café end)
Enum.map(items, fn café -> café end)
Enum.map(items, fn x -> x end); Enum.map(items, fn x -> x end)
"##;
    let expected = [1, 2, 3, 9, 10, 10];
    for engine in [&strict, &fast] {
        let found = lint(engine, "a.ex", source).unwrap();
        assert_eq!(found.iter().map(|f| f.line).collect::<Vec<_>>(), expected);
        for finding in &found {
            let token = &source[finding.start_byte..finding.end_byte];
            assert!(matches!(token, "map" | "\"m\\x61p\""), "{token}");
        }
    }
    assert_eq!(lint(&strict, "a.ex", source), lint(&fast, "a.ex", source));
}

#[test]
fn patterns_keep_suppression_scope_filters_and_exact_range_deduplication() {
    let rule = r#"id = "identity"
message = "m"
pattern = ['Enum.map(_, fn x -> x end)', 'Enum.map(_, fn x -> x end)']
report = "callee"
include = ['lib/**']
[compat]
credo = 'Legacy.Identity'
"#;
    let (root, strict) = project("[compat]\ncredo_directives = true\n", &[rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = "# fino:disable-next-line identity\nEnum.map(items, fn x -> x end)\n\
# credo:disable-for-next-line Legacy.Identity\nEnum.map(items, fn x -> x end)\n\
Enum.map(items, fn x -> x end); Enum.map(items, fn x -> x end)\n";
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "lib/a.ex", source), [5, 5]);
        assert!(lines(engine, "other/a.ex", source).is_empty());
    }
}

#[test]
fn pattern_clause_counts_use_generated_stress_inputs_not_giant_rule_examples() {
    let rule = include_str!("../examples/patterns/.fino/rules/case-true-false.toml");
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for count in [3, 256, 1024] {
        let extras = (2..count)
            .map(|n| format!("pattern{n} -> :other\n"))
            .collect::<String>();
        let source = format!("case value do\ntrue -> :a\nfalse -> :b\n{extras}end");
        for engine in [&strict, &fast] {
            assert!(lines(engine, "a.ex", &source).is_empty(), "{count} clauses");
        }
    }
    // The readable two-clause example remains a positive control.
    assert_eq!(
        lines(&strict, "a.ex", "case value do true -> :a; false -> :b end"),
        [1]
    );
}

#[test]
fn control_flow_blocks_normalize_without_inspecting_wildcard_bodies() {
    let rules = [
        "id = 'single'\nmessage = 'm'\npattern = 'with _ <- _ do _ else ... end'\n",
        "id = 'passthrough'\nmessage = 'm'\npattern = 'with ... do _ else error -> error end'\nwhere = {error = 'variable'}\n",
        "id = 'unless'\nmessage = 'm'\npattern = 'unless _ do _ else _ end'\n",
    ];
    let (root, strict) = project("", &rules);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = "with {:ok, x} <- read() do x else :error -> :no; other -> other end\n\
with {:ok, x} <- read(), {:ok, y} <- read() do y else err -> err end\n\
with {:ok, x} <- read() do x end\n\
unless ready? do :wait else :go end\n\
unless ready?, do: :wait, else: :go\n\
unless ready?, do: :wait\n";
    for engine in [&strict, &fast] {
        let mut found: Vec<_> = lint(engine, "a.ex", source)
            .unwrap()
            .into_iter()
            .map(|f| (f.rule, f.line))
            .collect();
        found.sort_unstable();
        assert_eq!(
            found,
            [
                ("passthrough".into(), 2),
                ("single".into(), 1),
                ("unless".into(), 4),
                ("unless".into(), 5)
            ]
        );
    }
    assert_eq!(lint(&strict, "a.ex", source), lint(&fast, "a.ex", source));
}

#[test]
fn operator_roots_and_empty_map_literals_use_shared_typed_candidates() {
    let rules = [
        "id = 'zero'\nmessage = 'm'\npattern = '_ * 0'\nreport = 'operator'\n",
        "id = 'negative'\nmessage = 'm'\npattern = '!_'\nreport = 'operator'\n",
        "id = 'map'\nmessage = 'm'\npattern = 'Enum.into(_, %{})'\nreport = 'callee'\n",
    ];
    let (root, strict) = project("", &rules);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = "count * 0\ncount * 0x0\ncount * (0)\ncount * 1\n!ready\nnot ready\n\
Enum.into(items, %{})\nitems |> Enum.into(%{})\nEnum.into(items, %{existing: true})\n";
    for engine in [&strict, &fast] {
        let found = lint(engine, "a.ex", source).unwrap();
        let mut actual: Vec<_> = found
            .iter()
            .map(|f| (f.rule.as_str(), f.line, &source[f.start_byte..f.end_byte]))
            .collect();
        actual.sort_unstable();
        assert_eq!(
            actual,
            [
                ("map", 7, "into"),
                ("map", 8, "into"),
                ("negative", 5, "!"),
                ("zero", 1, "*"),
                ("zero", 2, "*"),
                ("zero", 3, "*")
            ]
        );
    }
    assert_eq!(lint(&strict, "a.ex", source), lint(&fast, "a.ex", source));
}

#[test]
fn sequence_ellipsis_matches_fixed_edges_without_backtracking() {
    let rules = [
        "id = 'any'\nmessage = 'm'\npattern = 'any(...)'\n",
        "id = 'edges'\nmessage = 'm'\npattern = 'edges(value, ..., value)'\n",
        "id = 'list'\nmessage = 'm'\npattern = 'list([:first, ..., :last])'\n",
    ];
    let (root, strict) = project("", &rules);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = "any()\nany(a, b, c)\nxs |> any()\n\
edges(1, 1)\nedges(1, nested(a), 1)\nedges(1)\nedges(1, 2)\n\
list([:first, :last])\nlist([:first, nested(a), :last])\nlist([:last, :first])\n";
    for engine in [&strict, &fast] {
        let mut found = lines(engine, "a.ex", source);
        found.sort_unstable();
        assert_eq!(found, [1, 2, 3, 4, 5, 8, 9]);
    }
    assert_eq!(lint(&strict, "a.ex", source), lint(&fast, "a.ex", source));
}

#[test]
fn sequence_ellipsis_does_not_visit_skipped_expression_trees() {
    let rule = "id = 'edges'\nmessage = 'm'\npattern = 'check(:first, ..., :last)'\n";
    let engine = engine(&[rule]);
    let deep = format!("{}value{}", "{".repeat(300), "}".repeat(300));
    let middle = std::iter::repeat_n(deep, 256)
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        lines(&engine, "a.ex", &format!("check(:first, {middle}, :last)")),
        [1]
    );
    assert!(lines(&engine, "a.ex", "check(:last, :first)").is_empty());
}

#[test]
fn pattern_validation_rejects_unsupported_or_mixed_syntax_instead_of_guessing() {
    for spec in [
        "pattern = []",
        "pattern = 'Enum.map(..., ...)'",
        "pattern = 'check(_ + ...)'",
        "pattern = 'Enum.map(%{x: value}, _)'",
        "pattern = 'Enum.map(_, fn x ->'",
        "pattern = 'check(1'",
        "pattern = 'def foo do :ok end'",
        "pattern = '@name value'",
        "pattern = '_ * 0'\nreport = 'callee'",
        "pattern = 'check(_)'\nreport = 'operator'",
        "pattern = 'Enum.map(_, _)'\nquery = '(call) @finding'",
        "pattern = 'Enum.map(_, _)'\nregex = 'map'",
        "pattern = 'Enum.map(_, _)'\n[[stages]]\nfrom = 'finding'\nquery = '(call)'",
        "pattern = 'Enum.map(_, _)'\nwhere = { missing = 'variable' }",
        "pattern = 'Enum.map(_, _)'\nwhere = { _ = 'variable' }",
        "pattern = ['check(x)', 'check(y)']\nwhere = { x = 'variable' }",
        "query = '(call) @finding'\nwhere = { x = 'variable' }",
        "query = '(call) @finding'\nreport = 'callee'",
    ] {
        let root = fixture();
        let path = root.join("invalid.toml");
        fs::write(&path, format!("id = 'invalid'\nmessage = 'm'\n{spec}\n")).unwrap();
        let (rule, problems) = compile_rule(&path);
        assert!(
            rule.is_none() && !problems.errors.is_empty(),
            "{spec}: {problems:?}"
        );
    }
}

#[test]
fn source_order_comparisons_check_adjacent_lines_and_fold_ascii_case() {
    let rule = r#"
id = "order"
message = "m"
query = '(source (identifier) @_left . (identifier) @finding)'
[[comparisons]]
left = "_left"
right = "finding"
operator = "next-line"
[[comparisons]]
left = "_left"
right = "finding"
operator = "ascii-case-gt"
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    for engine in [&strict, &fast] {
        let source = "zebra\naPple\n\nzebra\n\napple\naBout\n";
        assert_eq!(lines(engine, "a.ex", source), [2, 7]);
        assert!(lines(engine, "a.ex", "zebra; aPple\n").is_empty());
    }
}

#[test]
fn atom_constraint_matches_decoded_static_atoms_without_matching_strings() {
    let rule = r#"
id = "same-atom"
message = "m"
pattern = 'sample(value, value)'
where = { value = "atom" }
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = r##"sample(:"ok", :ok)
sample(nil, :"nil")
sample("ok", "ok")
sample(:"#{dynamic}", :"#{dynamic}")
sample(:ok, :error)
"##;
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "a.ex", source), [1, 2]);
    }
}

#[test]
fn logical_list_count_treats_keyword_pairs_as_individual_items() {
    let rules = [
        r#"id = "list"
message = "m"
query = '((call (arguments [(list) (keywords)] @_list)) @finding (#list-items-gt? @_list 2))'
"#,
        r#"id = "stage-list"
message = "m"
query = '(call (arguments (_) @_slot)) @finding'
[[stages]]
from = "_slot"
query = '([(list) (keywords)] @_list (#list-items-gt? @_list 2))'
export = ["_list"]
"#,
    ];
    let (root, strict) = project("", &rules);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source =
        "f([:a, b: [1, 2, 3]])\nf([:a, b: 1, # comment\nc: 2])\nf(a: 1, b: 2, c: 3)\nf([1, 2])\n";
    for engine in [&strict, &fast] {
        let hits = lint(engine, "a.ex", source).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.rule.as_str(), hit.line))
                .collect::<Vec<_>>(),
            [
                ("list", 2),
                ("list", 4),
                ("stage-list", 2),
                ("stage-list", 4)
            ]
        );
    }
}

#[test]
fn line_span_predicate_matches_strict_grouped_and_auxiliary_queries() {
    let rules = [
        r#"id = "span"
message = "m"
query = '((call (do_block) @_body) @finding (#line-span-gt? @_body 2))'
"#,
        r#"id = "witness"
message = "m"
query = '(call (do_block) @_body) @finding'
[[conditions]]
capture = "_body"
relation = "root"
query = '((do_block) @_body (#line-span-gt? @_body 2))'
expect = "exists"
"#,
    ];
    let (root, strict) = project("", &rules);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let source = "quote do :ok end\nquote do\n  :ok\nend\nquote do\n  # comment\n\n  :ok\nend\n";
    for engine in [&strict, &fast] {
        let hits = lint(engine, "a.ex", source).unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| (hit.rule.as_str(), hit.line))
                .collect::<Vec<_>>(),
            [("span", 5), ("witness", 5)]
        );
    }
    assert!(
        crate::engine::compile_query("((call) @finding (#line-span-gt? @finding -1))", true)
            .is_err()
    );
}

#[test]
fn intermediate_pattern_hits_are_bounded_even_when_later_suppressed() {
    let rules: Vec<_> = (0..256)
        .map(|n| format!("id = 'probe-{n}'\nmessage = 'm'\npattern = 'p()'\n"))
        .collect();
    let refs: Vec<_> = rules.iter().map(String::as_str).collect();
    let (root, strict) = project("", &refs);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    let source = "p() # fino:disable-line all\n".repeat(400);
    for engine in [&strict, &fast] {
        let error = lint(engine, "a.ex", &source).unwrap_err();
        assert!(
            error.message.contains("intermediate pattern hit limit"),
            "{error:?}"
        );
    }
}

#[test]
fn pattern_limits_and_unsupported_repeated_capture_equality_fail_the_file() {
    let rule = "id = 'same'\nmessage = 'm'\npattern = 'check(value, value)'\n";
    let engine = engine(&[rule]);
    let source = "check(%{a: 1}, %{a: 1})";
    assert!(
        lint(&engine, "a.ex", source)
            .unwrap_err()
            .message
            .contains("repeated capture equality")
    );
    let source = format!("check({}x{}, x)", "(".repeat(200), ")".repeat(200));
    assert_eq!(lines(&engine, "a.ex", &source), [1]);
    let source = format!("check({}x{}, x)", "{".repeat(200), "}".repeat(200));
    assert!(
        lint(&engine, "a.ex", &source)
            .unwrap_err()
            .message
            .contains("nesting limit")
    );
}

#[test]
fn variable_capture_constraints_reject_literals_destructuring_and_calls() {
    let engine = engine(&[include_str!(
        "../examples/patterns/.fino/rules/identity-map.toml"
    )]);
    let source = "Enum.map(items, fn %{id: id} -> %{id: id} end)\n\
Enum.map(items, fn 1 -> 1 end)\n\
Enum.map(items, fn item -> item() end)\n\
Enum.map(items, fn item -> item end)\n";
    assert_eq!(lines(&engine, "a.ex", source), [4]);
}

#[test]
fn pattern_wildcards_do_not_normalize_deep_inputs_or_unrelated_pipelines() {
    let identity = include_str!("../examples/patterns/.fino/rules/identity-map.toml");
    let boolean_case = include_str!("../examples/patterns/.fino/rules/case-true-false.toml");
    let (root, strict) = project("", &[identity, boolean_case]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    let deep = format!("{}x{}", "{".repeat(300), "}".repeat(300));
    let source = format!(
        "seed{}\nEnum.map({deep}, fn x -> x end)\ncase ready? do true -> {deep}; false -> :no end\n",
        " |> unrelated()".repeat(500)
    );
    for engine in [&strict, &fast] {
        let mut found = lines(engine, "a.ex", &source);
        found.sort_unstable();
        assert_eq!(found, [2, 3]);
    }
}

#[test]
fn pattern_names_normalize_identifiers_but_preserve_quoted_values_and_token_spans() {
    let rules = [
        "id = 'local'\nmessage = 'm'\npattern = 'café(_)'\nreport = 'callee'\n",
        "id = 'remote'\nmessage = 'm'\npattern = 'Enum.café(_)'\nreport = 'callee'\n",
    ];
    let (root, strict) = project("", &rules);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    let source = "cafe\u{301}(x)\nx |> ((cafe\u{301}))\n\
Enum.cafe\u{301}(x)\nx |> Enum.cafe\u{301}()\n\
Enum.\"café\"(x)\nEnum.\"cafe\u{301}\"(x)\n";
    for engine in [&strict, &fast] {
        let found = lint(engine, "a.ex", source).unwrap();
        assert_eq!(
            found.iter().map(|f| f.line).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        for finding in found {
            assert!(matches!(
                &source[finding.start_byte..finding.end_byte],
                "cafe\u{301}" | "\"café\""
            ));
        }
    }
}

#[test]
fn variable_pattern_captures_exclude_elixir_context_constants() {
    let rule =
        "id = 'variable'\nmessage = 'm'\npattern = 'check(value)'\nwhere = {value = 'variable'}\n";
    let engine = engine(&[rule]);
    for constant in [
        "__MODULE__",
        "__ENV__",
        "__DIR__",
        "__CALLER__",
        "__STACKTRACE__",
    ] {
        assert!(lines(&engine, "a.ex", &format!("check({constant})")).is_empty());
    }
    assert_eq!(lines(&engine, "a.ex", "check(module)"), [1]);
}

#[test]
fn decoded_control_callee_names_match_direct_and_piped_calls() {
    for escape in [r"\b", r"\f", r"\0", r"\u0001", r"\t", r"\n", r"\\", r#"\""#] {
        let rule = format!(
            "id = 'control'\nmessage = 'm'\npattern = 'Enum.\"f{escape}\"(_)'\nreport = 'callee'\n"
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
        let source = format!("Enum.\"f{escape}\"(x)\nx |> Enum.\"f{escape}\"()\n");
        for engine in [&strict, &fast] {
            assert_eq!(lines(engine, "a.ex", &source), [1, 2], "{escape}");
        }
    }
}

#[test]
fn decoded_names_and_atoms_work_in_strict_and_grouped_queries() {
    let rules = [
        r#"id = "names"
message = "m"
query = '''
((dot right: (_) @finding) (#name-any-of? @finding "filter" "reject"))
'''
"#,
        r#"id = "atoms"
message = "m"
query = '''
((_) @finding (#atom-any-of? @finding "false" "get" "do"))
'''
"#,
    ];
    let source = r#"Enum.filter(xs)
Enum."f\x69lter"(xs)
Enum.'rej\u{65}ct'(xs)
Enum."fil#{suffix}"(xs)
false
:false
:"f\u0061lse"
"false"
:get
:"\get"
f(do: value)
f("d\x6f": value)
f("d#{suffix}": value)
"#;
    let (root, strict) = project("", &rules);
    fs::write(root.join("a.ex"), source).unwrap();
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let a = strict.scan(&root, None, 2, &Snapshot::default()).unwrap();
    let b = fast.scan(&root, None, 2, &Snapshot::default()).unwrap();
    assert!(a.errors.is_empty(), "{:?}", a.errors);
    assert_eq!(a.findings, b.findings);
    assert_eq!(
        a.findings.iter().map(|f| f.line).collect::<Vec<_>>(),
        [1, 2, 3, 5, 6, 7, 9, 10, 11, 12]
    );
}

#[test]
fn decoded_predicates_apply_in_stages_and_conditions_and_validate_arguments() {
    let rule = r#"id = "names"
message = "m"
query = '(call target: (dot right: (_) @_name) (arguments (_) @_value)) @finding'
[[stages]]
from = "_name"
query = '((_) @_name (#name-eq? @_name "filter"))'
[[conditions]]
capture = "_value"
query = '((_) @_atom (#atom-eq? @_atom "false"))'
expect = "exists"
"#;
    let (root, strict) = project("", &[rule]);
    let source =
        "Enum.\"f\\x69lter\"(:\"f\\u0061lse\")\nEnum.reject(false)\nEnum.filter(\"false\")\n";
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "a.ex", source), [1]);
    }
    for predicate in [
        "#name-eq? @finding",
        "#name-eq? @finding @finding",
        "#atom-eq? @finding \"a\" \"b\"",
        "#atom-any-of? @finding",
        "#name-any-of? \"filter\" \"filter\"",
    ] {
        let path = root.join("invalid.toml");
        fs::write(
            &path,
            format!("id = 'invalid'\nmessage = 'm'\nquery = '((_) @finding ({predicate}))'\n"),
        )
        .unwrap();
        assert!(compile_rule(&path).0.is_none(), "{predicate}");
    }
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
fn shared_query_compilation_does_not_reuse_stale_rules_across_engine_loads() {
    let (root, first) = project(
        "",
        &[
            "id = 'marker'\nmessage = 'Marker.'\nquery = '((identifier) @finding (#eq? @finding \"first\"))'\n",
        ],
    );
    assert_eq!(lines(&first, "a.ex", "first\nsecond\n"), [1]);
    fs::write(root.join(".fino/rules/marker.toml"),
        "id = 'marker'\nmessage = 'Marker.'\nquery = '((identifier) @finding (#eq? @finding \"second\"))'\n"
    ).unwrap();
    for (engine, problems) in [
        Engine::load(&root.join(".fino/config.toml"), None).unwrap(),
        Engine::load_fast(&root.join(".fino/config.toml")).unwrap(),
    ] {
        assert!(problems.iter().all(|p| p.errors.is_empty()));
        assert_eq!(lines(&engine, "a.ex", "first\nsecond\n"), [2]);
    }
    assert_eq!(lines(&first, "a.ex", "first\nsecond\n"), [1]);
}

const SCOPED: &str = r#"
id = "scoped"
message = "Scoped match."
query = '''((call target: (identifier) @finding (arguments) @_scope)
  (#eq? @finding "box"))'''
[[conditions]]
capture = "_scope"
query = '((identifier) @value (#eq? @value "marker"))'
expect = "exists"
[[conditions]]
capture = "_scope"
query = '((identifier) @value (#eq? @value "blocked"))'
expect = "absent"
"#;

#[test]
fn capture_conditions_are_scoped_anded_and_identical_in_grouped_queries() {
    let other = "id = 'other'\nmessage = 'Other.'\nquery = '(atom) @finding'\n";
    let (root, strict) = project("", &[SCOPED, other]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let text = "marker\nbox([ordinary])\nbox([marker, marker])\nbox([marker, blocked])\nbox([marker, nest([deep])])\nblocked\n";
    let expected = lint(&strict, "a.ex", text).unwrap();
    assert_eq!(
        expected.iter().map(|row| row.line).collect::<Vec<_>>(),
        [3, 5]
    );
    assert_eq!(expected, lint(&fast, "a.ex", text).unwrap());
    let nested = "box([\n marker,\n box([ordinary])\n])\nbox([\n blocked,\n box([marker])\n])\n";
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "nested.ex", nested), [1, 7]);
    }
    // Cached witnesses and captures are local to one source file.
    assert!(lint(&fast, "b.ex", "box([ordinary])\n").unwrap().is_empty());
}

#[test]
fn capture_conditions_include_self_and_root_does_not_search_descendants() {
    let rule = r#"
id = "rooted"
message = "Root only."
query = '((call target: (identifier) @finding (arguments (_) @_scope)) (#eq? @finding "box"))'
[[conditions]]
capture = "_scope"
relation = "root"
query = '((identifier) @value (#eq? @value "marker"))'
expect = "exists"
"#;
    for (relation, expected) in [("root", vec![1]), ("descendant-or-self", vec![1, 2])] {
        let rule = rule.replace("relation = \"root\"", &format!("relation = \"{relation}\""));
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert_eq!(
                lines(
                    engine,
                    "a.ex",
                    "box(marker)\nbox([marker])\nbox(ordinary)\n"
                ),
                expected
            );
        }
    }
}

#[test]
fn capture_conditions_reject_missing_and_repeated_captures() {
    for (query, text, expected) in [
        (
            "(list) @_scope @finding\n(tuple) @finding",
            "[x]\n{x}\n",
            vec![1],
        ),
        (
            "(list . (identifier) @_scope+ .) @finding",
            "[x]\n[x, y]\n",
            vec![1],
        ),
    ] {
        for expect in ["exists", "absent"] {
            let auxiliary = if expect == "exists" {
                "(identifier)"
            } else {
                "(atom)"
            };
            let rule = format!(
                "id = 'cardinality'\nmessage = 'One scope.'\nquery = '''{query}'''\n[[conditions]]\ncapture = '_scope'\nquery = '{auxiliary}'\nexpect = '{expect}'\n"
            );
            let (root, strict) = project("", &[&rule]);
            let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
            for engine in [&strict, &fast] {
                assert_eq!(lines(engine, "a.ex", text), expected, "{expect}: {query}");
            }
        }
    }
}

#[test]
fn capture_conditions_evaluate_auxiliary_count_predicates_and_keep_captures_local() {
    let rule = r#"
id = "counts"
message = "One item."
query = '(list) @finding @_scope'
[[conditions]]
capture = "_scope"
query = '((list) @finding (#count-eq? @finding 1))'
relation = "root"
expect = "exists"
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "a.ex", "[x]\n[x, y]\n[ # note\nx]\n"), [1, 3]);
    }
}

#[test]
fn capture_conditions_validate_both_loading_paths() {
    let cases = [
        (
            "capture = 'missing'\nquery = '(identifier)'\nexpect = 'exists'",
            "unknown outer capture",
        ),
        (
            "capture = '_scope'\nquery = ''\nexpect = 'exists'",
            "at least one pattern",
        ),
        (
            "capture = '_scope'\nquery = '(not_a_node)'\nexpect = 'exists'",
            "invalid query",
        ),
        (
            "capture = '_scope'\nquery = '((identifier) @a (#unknown? @a))'\nexpect = 'exists'",
            "unsupported predicate",
        ),
        (
            "capture = '_scope'\nquery = '((identifier) @a (identifier) @b)'\nexpect = 'exists'",
            "single root",
        ),
        (
            "capture = '_scope'\nquery = '(identifier)'\nexpect = 'maybe'",
            "unknown variant",
        ),
    ];
    for (condition, error) in cases {
        let (root, _) = project("", &[]);
        fs::write(root.join(".fino/rules/invalid.toml"), format!("id = 'invalid'\nmessage = 'Invalid.'\nquery = '(list) @finding @_scope'\n[[conditions]]\n{condition}\n")).unwrap();
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
                    .any(|message| message.contains(error)),
                "{problems:?}"
            );
        }
    }
    for matcher in ["regex = 'x'", "path_only = true"] {
        let (root, _) = project("", &[]);
        let path = root.join("invalid.toml");
        fs::write(&path, format!("id = 'invalid'\nmessage = 'Invalid.'\n{matcher}\n[[conditions]]\ncapture = 'finding'\nquery = '(identifier)'\nexpect = 'exists'\n")).unwrap();
        let (_, problems) = compile_rule(&path);
        assert!(
            problems
                .errors
                .iter()
                .any(|message| message.contains("conditions require a valid outer query"))
        );
    }
}

#[test]
fn capture_condition_match_limit_errors_are_not_absence_or_findings() {
    for expect in ["exists", "absent"] {
        let rule = format!(
            "id = 'overflow'\nmessage = 'Overflow.'\nquery = '(list) @finding @_scope'\n[[conditions]]\ncapture = '_scope'\nquery = '(list (identifier) @a (identifier) @b)'\nexpect = '{expect}'\n"
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        let text = format!("[{}]\n", vec!["item"; 50].join(", "));
        for engine in [&strict, &fast] {
            let (mut parser, mut cursor) = scan::worker();
            cursor.set_match_limit(1);
            let error =
                scan::source(engine, None, &mut parser, &mut cursor, "a.ex", &text).unwrap_err();
            assert!(
                error
                    .message
                    .contains("capture condition query match limit exceeded"),
                "{error:?}"
            );
        }
    }
}

const STAGED: &str = r#"
id = "staged"
message = "Equal items."
query = '''((call target: (identifier) @finding
  (arguments (_) @_expression)) (#eq? @finding "box"))'''
[[stages]]
from = "_expression"
unwrap_singleton = ["block"]
query = '(list . (_) @_first . (_) @_second .)'
export = ["_first", "_second"]
[[stages]]
from = "_first"
unwrap_singleton = ["block"]
query = '(identifier) @_left'
export = ["_left"]
[[stages]]
from = "_second"
unwrap_singleton = ["block"]
query = '(identifier) @_right'
export = ["_right"]
[[comparisons]]
left = "_left"
right = "_right"
operator = "text-eq"
[[conditions]]
capture = "_left"
relation = "root"
query = '((identifier) @local (#not-eq? @local "blocked"))'
expect = "exists"
"#;

const BRANCHED: &str = r#"
id = "branched"
message = "One logical rule."
[compat]
credo = "Credo.Check.Warning.Example"

[[branches]]
query = '''((call target: (identifier) @_name (arguments (_) @_value))
    (#eq? @_name "box"))'''
[[branches.stages]]
from = "_value"
unwrap_singleton = ["block"]
query = '(integer) @finding'
export = ["finding"]

[[branches]]
query = '''((call target: (identifier) @finding (arguments (_) @_value))
    (#eq? @finding "box"))'''
[[branches.conditions]]
capture = "_value"
relation = "root"
query = '(string)'
expect = "exists"

[[branches]]
query = '''
; Multiple patterns and leading comments must still map to this branch.
((integer) @finding (#eq? @finding "7"))
((float) @finding (#eq? @finding "7.0"))
'''
"#;

#[test]
fn alternative_pipelines_are_isolated_and_deduplicate_only_identical_ranges() {
    let (root, strict) = project("", &[BRANCHED, DEBUG]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let text = "box(7)\nbox(\"yes\")\nother(8)\n7\nbox(7); box(7)\n7.0\nbox(:no)\n";
    let expected = lint(&strict, "lib/a.ex", text).unwrap();
    assert_eq!(
        expected
            .iter()
            .map(|f| (f.line, &text[f.start_byte..f.end_byte]))
            .collect::<Vec<_>>(),
        [
            (1, "7"),
            (2, "box"),
            (4, "7"),
            (5, "7"),
            (5, "7"),
            (6, "7.0")
        ]
    );
    assert_eq!(lint(&fast, "lib/a.ex", text).unwrap(), expected);
    fs::write(root.join("a.ex"), text).unwrap();
    for engine in [&strict, &fast] {
        for threads in [1, 2] {
            let report = engine
                .scan(&root, None, threads, &Snapshot::default())
                .unwrap();
            assert!(report.errors.is_empty());
            assert_eq!(report.findings.len(), 6);
        }
    }
}

#[test]
fn branch_rules_share_filters_limits_and_suppression_identity() {
    let limited = BRANCHED.replace(
        "[compat]",
        "limit = 'first-per-file'\ninclude = ['lib/**']\n[compat]",
    );
    let config = "[compat]\ncredo_directives = true\n";
    for rule in [BRANCHED.to_string(), limited] {
        let (root, strict) = project(config, &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert!(
                lint(
                    engine,
                    "lib/a.ex",
                    "# fino:disable-file branched\nbox(7)\nbox(\"yes\")\n"
                )
                .unwrap()
                .is_empty()
            );
            assert!(lint(engine, "lib/a.ex", "# credo:disable-for-this-file Credo.Check.Warning.Example\nbox(7)\nbox(\"yes\")\n").unwrap().is_empty());
            assert!(
                lint(
                    engine,
                    "lib/a.ex",
                    "# fino:disable-next-line branched\nbox(7)\n"
                )
                .unwrap()
                .is_empty()
            );
            assert_eq!(
                lines(engine, "lib/a.ex", "box(7)\nbox(\"yes\")\n"),
                if rule.contains("first-per-file") {
                    vec![1]
                } else {
                    vec![1, 2]
                }
            );
            if rule.contains("first-per-file") {
                assert!(lint(engine, "other/a.ex", "box(7)\n").unwrap().is_empty());
            }
        }
    }
}

#[test]
fn branch_validation_rejects_mixed_matchers_and_cross_branch_bindings() {
    for (body, expected) in [
        (
            "query = '(integer) @finding'\n[[branches]]\nquery = '(float) @finding'",
            "exactly one",
        ),
        (
            "regex = 'x'\n[[branches]]\nquery = '(integer) @finding'",
            "exactly one",
        ),
        ("branches = []", "exactly one"),
        (
            "[[conditions]]\ncapture = 'finding'\nquery = '(integer)'\nexpect = 'exists'\n[[branches]]\nquery = '(integer) @finding'",
            "inside each branch",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding'\n[[branches]]\nquery = '(not_a_node) @finding'",
            "branches[1]: invalid query",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding ('\n[[branches]]\nquery = '(float) @finding) (atom) @finding'",
            "invalid query",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding @_private'\n[[branches]]\nquery = '((float) @finding (#eq? @_private \"7\"))'",
            "branches[1]: invalid query",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding'\n[[branches]]\nquery = '(float) @_leaked'",
            "branches[1]: query needs",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding'\n[[branches.conditions]]\ncapture = '_leaked'\nquery = '(integer)'\nexpect = 'exists'\n[[branches]]\nquery = '(float) @finding @_leaked'",
            "branches[0]: conditions[0].capture",
        ),
        (
            "[[branches]]\nquery = '(integer) @finding'\nmessage = 'ignored'",
            "unknown field",
        ),
    ] {
        let root = fixture();
        let dir = root.join(".fino/rules");
        fs::create_dir_all(&dir).unwrap();
        fs::write(root.join(".fino/config.toml"), "").unwrap();
        fs::write(
            dir.join("bad.toml"),
            format!("id = 'bad'\nmessage = 'Bad.'\n{body}\n"),
        )
        .unwrap();
        for load in [
            Engine::load(&root.join(".fino/config.toml"), None),
            Engine::load_fast(&root.join(".fino/config.toml")),
        ] {
            let (engine, problems) = load.unwrap();
            assert!(
                engine.groups.is_empty(),
                "invalid branches must not execute: {body}"
            );
            assert!(
                problems
                    .iter()
                    .flat_map(|p| &p.errors)
                    .any(|e| e.contains(expected)),
                "{body}: {problems:?}"
            );
        }
    }
}

#[test]
fn branch_work_errors_fail_the_whole_file_even_after_another_branch_matches() {
    let rule = r#"
id = 'bounded-branches'
message = 'Bounded.'
[[branches]]
query = '(integer) @finding'
[[branches]]
query = '(list . (_) @_left . (_) @_right .) @finding'
[[branches.comparisons]]
left = '_left'
right = '_right'
operator = 'text-eq'
left_prefix = 'x'
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    let text = format!(
        "1\n[\"{}\", \"{}\"]\n",
        "a".repeat(500_000),
        "a".repeat(500_000)
    );
    for engine in [&strict, &fast] {
        let error = lint(engine, "a.ex", &text).expect_err("partial findings are not success");
        assert!(error.message.contains("predicate work limit exceeded"));
    }
}

#[test]
fn stages_can_create_findings_at_root_or_descendant_token_spans() {
    // Query groups order longer queries first. Force capture-index renumbering.
    let other = format!(
        "id = 'aaa'\nmessage = 'Other.'\nquery = ''';; {}\n(atom) @_unrelated @finding'''\n",
        "padding ".repeat(40)
    );
    let rule = r#"
id = "created"
message = "Selected span."
query = '''((call target: (identifier) @_target (arguments (_) @_expression))
  (#eq? @_target "box"))'''
[[stages]]
from = "_expression"
unwrap_singleton = ["block"]
query = '''(alias) @finding @_name
(dot operator: "." @finding right: (alias) @_name)'''
export = ["finding", "_name"]
[[conditions]]
capture = "_name"
relation = "root"
query = '((alias) @value (#eq? @value "Member"))'
expect = "exists"
"#;
    let text = "box(((\n Member\n)))\nbox((prefix().\n Member))\nbox(wrap(Member))\nbox(Other)\n";
    for rules in [vec![rule], vec![rule, &other]] {
        let (root, strict) = project("", &rules);
        let (fast, problems) = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| Engine::load_fast(&root.join(".fino/config.toml")))
            .unwrap();
        assert_eq!(fast.groups.len(), 1);
        if rules.len() > 1 {
            assert_eq!(
                fast.groups[0].query.capture_index_for_name("finding"),
                Some(1)
            );
        }
        assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
        for engine in [&strict, &fast] {
            let rows = lint(engine, "a.ex", text).unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| (row.line, row.column))
                    .collect::<Vec<_>>(),
                [(2, 2), (4, 14)]
            );
            assert_eq!(
                rows.iter()
                    .map(|row| &text[row.start_byte..row.end_byte])
                    .collect::<Vec<_>>(),
                ["Member", "."]
            );
        }
    }
    // A later stage may reject a newly created finding before anything is emitted.
    let rejects = rule.replace(
        "[[conditions]]",
        "[[stages]]\nfrom = '_name'\nquery = '(identifier)'\n[[conditions]]",
    );
    let (root, strict) = project("", &[&rejects]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        assert!(lint(engine, "a.ex", text).unwrap().is_empty());
    }
}

#[test]
fn stages_require_a_final_finding_without_weakening_ordinary_query_validation() {
    for extension in ["", "[[stages]]\nfrom = '_scope'\nquery = '(list)'\n"] {
        let (root, _) = project("", &[]);
        fs::write(root.join(".fino/rules/invalid.toml"), format!("id = 'invalid'\nmessage = 'Missing finding.'\nquery = '(list) @_scope'\n{extension}\n")).unwrap();
        for strict in [true, false] {
            let (_, problems) = if strict {
                Engine::load(&root.join(".fino/config.toml"), None)
            } else {
                Engine::load_fast(&root.join(".fino/config.toml"))
            }
            .unwrap();
            let expected = if extension.is_empty() {
                "query needs a @finding capture"
            } else {
                "final binding `finding` is required"
            };
            assert!(
                problems
                    .iter()
                    .flat_map(|p| &p.errors)
                    .any(|error| error.contains(expected)),
                "{problems:?}"
            );
        }
    }
}

#[test]
fn root_stages_support_alternatives_and_captures_below_the_pattern_root() {
    let rule = r#"
id = "alternatives"
message = "Root selection."
query = '''((call target: (identifier) @finding (arguments (_) @_expression))
  (#eq? @finding "box"))'''
[[stages]]
from = "_expression"
query = '''[(identifier) @part (alias) @part
  (call target: (identifier) @part)]'''
export = ["part"]
[[conditions]]
capture = "part"
relation = "root"
query = '[(identifier) (alias)]'
expect = "exists"
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    for engine in [&strict, &fast] {
        assert_eq!(
            lines(
                engine,
                "a.ex",
                "box(token)\nbox(Member)\nbox(work(x))\nbox([token])\n"
            ),
            [1, 2, 3]
        );
    }
}

#[test]
fn stages_export_bindings_compare_text_and_preserve_outer_findings_in_both_modes() {
    let other = format!(
        "id = 'other'\nmessage = 'Other.'\nquery = ''';; {}\n(atom) @_unrelated @finding'''\n",
        "padding ".repeat(40)
    );
    let (root, strict) = project("", &[STAGED, &other]);
    let (fast, problems) = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(|| Engine::load_fast(&root.join(".fino/config.toml")))
        .unwrap();
    assert_eq!(fast.groups.len(), 1);
    assert_eq!(
        fast.groups[0].query.capture_index_for_name("finding"),
        Some(1)
    );
    assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    let text = "box([x, x])\nbox(([(x), ((x))]))\nbox([x, y])\nbox([blocked, blocked])\nbox([[x, x]])\nbox((work(); [x, x]))\nbox([x, x, x])\n";
    let expected = lint(&strict, "a.ex", text).unwrap();
    assert_eq!(
        expected.iter().map(|row| row.line).collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(
        expected
            .iter()
            .all(|row| &text[row.start_byte..row.end_byte] == "box")
    );
    assert_eq!(expected, lint(&fast, "a.ex", text).unwrap());
    for engine in [&strict, &fast] {
        assert!(lint(engine, "next.ex", "box([x, y])\n").unwrap().is_empty());
        assert_eq!(lines(engine, "nested.ex", "box([x, box([y, y])])\n"), [1]);
    }
}

#[test]
fn stages_unwrap_arbitrary_singletons_and_ignore_only_declared_children() {
    let deep = format!(
        "box({}[{}x{}, {}x{}]{})\n",
        "(".repeat(64),
        "(".repeat(40),
        ")".repeat(40),
        "(".repeat(35),
        ")".repeat(35),
        ")".repeat(64)
    );
    let comment = "box(( # kept comment\n [x, x]\n))\n";
    for ignore_comments in [true, false] {
        let rule = if ignore_comments {
            STAGED.to_string()
        } else {
            STAGED.replace(
                "unwrap_singleton = [\"block\"]",
                "unwrap_singleton = [\"block\"]\nignore_children = []",
            )
        };
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert_eq!(lines(engine, "deep.ex", &deep), [1]);
            assert_eq!(
                lines(engine, "comments.ex", comment),
                if ignore_comments { vec![1] } else { vec![] }
            );
        }
    }
}

#[test]
fn stage_queries_keep_local_equality_and_comparisons_can_reject_equal_text() {
    let rule = r#"
id = "local"
message = "Local equality."
query = '(list) @finding @_scope'
[[stages]]
from = "_scope"
query = '((list . (identifier) @a . (identifier) @b .) (#eq? @a @b))'
export = []
"#;
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "a.ex", "[x, x]\n[x, y]\n"), [1]);
    }
    let unequal = STAGED.replace("operator = \"text-eq\"", "operator = \"text-not-eq\"");
    let (root, strict) = project("", &[&unequal]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        assert_eq!(lines(engine, "a.ex", "box([x, x])\nbox([x, y])\n"), [2]);
    }
}

#[test]
fn character_tokens_accept_literal_controls_without_rewriting_source_bytes() {
    let rule = "id = 'character'\nmessage = 'Character.'\nquery = '(char) @finding'\n";
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        for value in [0_u8, 1, 9, 10, 27, 31] {
            for escaped in [false, true] {
                let token = format!("?{}{}", if escaped { "\\" } else { "" }, char::from(value));
                let text = format!("prefix = \"é\"\nvalue = {token}\n");
                let hits = lint(engine, "character.ex", &text).unwrap();
                assert_eq!(hits.len(), 1, "{value}, {escaped}");
                let hit = &hits[0];
                assert_eq!((hit.line, hit.column), (2, 9));
                assert_eq!(&text[hit.start_byte..hit.end_byte], token);
                assert_eq!(hit.end_byte - hit.start_byte, token.len());
            }
        }
        for text in ["value = ?\\", "value = ?\\\\"] {
            assert_eq!(lint(engine, "character.ex", text).unwrap().len(), 1);
        }
    }
}

#[test]
fn actual_eof_and_nul_outside_character_tokens_still_fail_closed() {
    let rule = "id = 'character'\nmessage = 'Character.'\nquery = '(char) @finding'\n";
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        for text in ["value = ?", "\0\n", "x\0y\n", "x * ?\0 +\n"] {
            let error = lint(engine, "character.ex", text).expect_err("invalid source must fail");
            assert!(
                matches!(
                    error.message.as_str(),
                    "Elixir syntax error or missing token" | "Elixir parse was incomplete"
                ),
                "{error:?}"
            );
        }
    }
}

#[test]
fn canonical_unicode_comparisons_are_opt_in_and_match_in_both_loading_modes() {
    let text = concat!(
        "[\"ascii\", \"ascii\"]\n",
        "[\"ascii\", \"ASCII\"]\n",
        "[\"café\", \"cafe\u{301}\"]\n",
        "[\"cafe\u{301}\", \"café\"]\n",
        "[\"x\u{301}\u{327}\", \"x\u{327}\u{301}\"]\n",
        "[\"가\", \"가\"]\n",
        "[\"ﬃ\", \"ffi\"]\n",
        "[\"café\", \"cafe\"]\n",
        "[\"cafe\u{301}\", \"cafe\u{301}\"]\n",
        "[\"é\", \"\\u00E9\"]\n",
    );
    for (operator, expected) in [
        ("text-eq", vec![1, 9]),
        ("text-not-eq", vec![2, 3, 4, 5, 6, 7, 8, 10]),
        ("nfc-eq", vec![1, 3, 4, 5, 6, 9]),
        ("nfc-not-eq", vec![2, 7, 8, 10]),
    ] {
        let rule = format!(
            "id = 'canonical'\nmessage = 'Canonical text.'\n\
             query = '(list . (_) @_left . (_) @_right .) @finding'\n\
             [[comparisons]]\nleft = '_left'\nright = '_right'\noperator = '{operator}'\n"
        );
        let other = "id = 'other'\nmessage = 'Other.'\nquery = '(integer) @finding'\n";
        let (root, strict) = project("", &[other, &rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            assert_eq!(lines(engine, "canonical.ex", text), expected, "{operator}");
        }
    }
}

#[test]
fn literal_prefixes_compose_with_byte_and_normalized_comparisons() {
    for (left_prefix, right_prefix, source, byte_equal, nfc_equal) in [
        ("pre_", "", "[\"name\", \"pre_name\"]\n", true, true),
        ("", "pre_", "[\"pre_name\", \"name\"]\n", true, true),
        ("same_", "same_", "[\"a\", \"a\"]\n", true, true),
        ("x_", "y_", "[\"name\", \"name\"]\n", false, false),
        ("a", "", "[\"\u{301}\", \"á\"]\n", false, true),
        ("", "a", "[\"á\", \"\u{301}\"]\n", false, true),
        ("e\u{301}", "é", "[\"name\", \"name\"]\n", false, true),
        ("P_", "p_", "[\"name\", \"name\"]\n", false, false),
    ] {
        for (operator, equal) in [
            ("text-eq", byte_equal),
            ("text-not-eq", !byte_equal),
            ("nfc-eq", nfc_equal),
            ("nfc-not-eq", !nfc_equal),
        ] {
            let rule = format!(
                "id = 'prefix'\nmessage = 'Prefixed text.'\n\
                 query = '(list . (string (quoted_content) @_left) . (string (quoted_content) @_right) .) @finding'\n\
                 [[comparisons]]\nleft = '_left'\nright = '_right'\noperator = '{operator}'\n\
                 left_prefix = {}\nright_prefix = {}\n",
                serde_json::to_string(left_prefix).unwrap(),
                serde_json::to_string(right_prefix).unwrap(),
            );
            let (root, strict) = project("", &[&rule]);
            let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
            for engine in [&strict, &fast] {
                assert_eq!(
                    lines(engine, "prefix.ex", source),
                    if equal { vec![1] } else { vec![] },
                    "{operator}, {left_prefix:?}, {right_prefix:?}",
                );
            }
        }
    }
}

#[test]
fn prefix_work_is_bounded_for_byte_and_normalized_comparisons() {
    for operator in ["text-eq", "nfc-eq"] {
        let rule = format!(
            "id = 'prefix-budget'\nmessage = 'Bounded prefixes.'\n\
             query = '(list . (_) @_left . (_) @_right .) @finding'\n\
             [[comparisons]]\nleft = '_left'\nright = '_right'\noperator = '{operator}'\n\
             left_prefix = '{}'\n",
            "p".repeat(1_000_000),
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        for engine in [&strict, &fast] {
            let error =
                lint(engine, "bounded.ex", "[x, x]\n").expect_err("prefix work must be bounded");
            assert!(error.message.contains("predicate work limit exceeded"));
        }
    }
}

#[test]
fn normalized_comparison_work_exhaustion_fails_the_file_in_both_modes() {
    let rule = "id = 'bounded'\nmessage = 'Bounded.'\n\
                query = '(list . (_) @_left . (_) @_right .) @finding'\n\
                [[comparisons]]\nleft = '_left'\nright = '_right'\noperator = 'nfc-eq'\n";
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    let item = "a".repeat(500_000);
    let text = format!("[\"{item}\", \"{item}\"]\n");
    for engine in [&strict, &fast] {
        let error = lint(engine, "bounded.ex", &text).expect_err("normalization must be bounded");
        assert!(error.message.contains("predicate work limit exceeded"));
    }
}

#[test]
fn stage_inputs_and_comparisons_reject_missing_or_repeated_outer_bindings() {
    for query in [
        "(list) @finding @_scope\n(tuple) @finding",
        "(list . (identifier) @_scope+ .) @finding",
    ] {
        for extension in [
            "[[stages]]\nfrom = '_scope'\nquery = '(_) @item'\nexport = ['item']",
            "[[comparisons]]\nleft = '_scope'\nright = '_scope'\noperator = 'text-eq'",
            "[[comparisons]]\nleft = '_scope'\nright = '_scope'\noperator = 'nfc-eq'",
        ] {
            let rule = format!(
                "id = 'cardinal'\nmessage = 'One binding.'\nquery = '''{query}'''\n{extension}\n"
            );
            let (root, strict) = project("", &[&rule]);
            let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
            let (text, expected) = if query.contains("tuple") {
                ("[x]\n{x}\n", vec![1])
            } else {
                ("[x]\n[x, y]\n", vec![1])
            };
            for engine in [&strict, &fast] {
                assert_eq!(lines(engine, "a.ex", text), expected);
            }
        }
    }
}

#[test]
fn stage_validation_rejects_collisions_forward_references_and_ambiguous_exports() {
    let cases = [
        (
            "[[stages]]\nfrom = 'later'\nquery = '(list) @later'\nexport = ['later']",
            "stages[0].from:",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @finding'\nexport = ['finding']",
            "stages[0].export[0]: binding",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @a'\nexport = ['a', 'a']",
            "stages[0].export[1]: binding",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @a'\nexport = ['missing']",
            "stages[0].export[0]: unknown local",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list (identifier) @a+)'\nexport = ['a']",
            "must occur exactly once",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list (identifier)? @a)'\nexport = ['a']",
            "must occur exactly once",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '''(list) @a\n(tuple)'''\nexport = ['a']",
            "must occur exactly once",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list)'\nunwrap_singleton = ['not_a_kind']",
            "stages[0].unwrap_singleton:",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list)'\nignore_children = ['not_a_kind']",
            "stages[0].ignore_children:",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = ''",
            "stages[0].query: query needs",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '((identifier) @a (identifier) @b)'",
            "stages[0].query: query patterns must have a single root",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @a'\nexport = ['a']\n[[stages]]\nfrom = 'a'\nquery = '(list) @a'\nexport = ['a']",
            "stages[1].export[0]: binding",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @local'\n[[stages]]\nfrom = 'local'\nquery = '(list)'",
            "stages[1].from:",
        ),
        (
            "[[stages]]\nfrom = '_scope'\nquery = '(list) @a'\nrelation = 'descendant-or-self'",
            "unknown variant",
        ),
        (
            "[[comparisons]]\nleft = 'missing'\nright = '_scope'\noperator = 'text-eq'",
            "comparisons[0].left:",
        ),
        (
            "[[comparisons]]\nleft = '_scope'\nright = 'missing'\noperator = 'text-eq'",
            "comparisons[0].right:",
        ),
    ];
    for (extension, expected) in cases {
        let (root, _) = project("", &[]);
        fs::write(root.join(".fino/rules/invalid.toml"), format!("id = 'invalid'\nmessage = 'Invalid.'\nquery = '(list) @finding @_scope'\n{extension}\n")).unwrap();
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
                "{expected}: {problems:?}"
            );
        }
    }
}

#[test]
fn ambiguous_stage_results_and_query_overflow_fail_the_file_in_both_modes() {
    let rule = "id = 'ambiguous'\nmessage = 'One result.'\nquery = '(list) @finding @_scope'\n[[stages]]\nfrom = '_scope'\nquery = '(list (identifier) @item)'\nexport = ['item']\n";
    let (root, strict) = project("", &[rule]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    for engine in [&strict, &fast] {
        let error = lint(engine, "a.ex", "[x, y]\n").unwrap_err();
        assert!(
            error
                .message
                .contains("stages[0]: query produced multiple matches"),
            "{error:?}"
        );
    }
    let overflow = rule.replace(
        "(list (identifier) @item)",
        "(list (identifier) @item (identifier) @other)",
    );
    let (root, strict) = project("", &[&overflow]);
    let (fast, _) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    let text = format!("[{}]\n", vec!["x"; 50].join(", "));
    for engine in [&strict, &fast] {
        let (mut parser, mut cursor) = scan::worker();
        cursor.set_match_limit(1);
        let error =
            scan::source(engine, None, &mut parser, &mut cursor, "a.ex", &text).unwrap_err();
        assert!(
            error
                .message
                .contains("stages[0]: query match limit exceeded"),
            "{error:?}"
        );
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

#[test]
fn auxiliary_rejected_candidate_work_fails_closed_without_match_overflow() {
    let query = "((list (identifier) @a (identifier) @b) @xs (#count-eq? @xs 0))";
    let text = format!("[{}]\n", vec!["item"; 500].join(", "));
    for extension in [
        format!("[[conditions]]\ncapture = '_scope'\nquery = '{query}'\nexpect = 'exists'"),
        format!("[[conditions]]\ncapture = '_scope'\nquery = '{query}'\nexpect = 'absent'"),
        format!("[[stages]]\nfrom = '_scope'\nquery = '{query}'"),
    ] {
        let rule = format!(
            "id = 'bounded'\nmessage = 'Bounded.'\nquery = '(list) @finding @_scope'\n{extension}\n"
        );
        let (root, strict) = project("", &[&rule]);
        let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
        assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
        for engine in [&strict, &fast] {
            let error = lint(engine, "a.ex", &text).unwrap_err();
            assert!(error.message.contains("auxiliary query"), "{error:?}");
            assert!(
                error.message.contains("limit exceeded")
                    || error.message.contains("deadline exceeded"),
                "{error:?}"
            );
            assert!(
                !error.message.contains("query match limit exceeded"),
                "{error:?}"
            );
        }
    }
}

#[test]
fn auxiliary_wildcard_parent_roots_are_explicitly_unsupported_not_assumed_siblings() {
    for extension in [
        "[[conditions]]\ncapture = '_scope'\nquery = '(_ (identifier) @item)'\nexpect = 'exists'",
        "[[stages]]\nfrom = '_scope'\nquery = '(_ (identifier) @item)'\nexport = ['item']",
    ] {
        let (root, _) = project("", &[]);
        fs::write(root.join(".fino/rules/invalid.toml"), format!("id = 'invalid'\nmessage = 'Explicit root.'\nquery = '(list) @finding @_scope'\n{extension}\n")).unwrap();
        for strict in [true, false] {
            let (_, problems) = if strict {
                Engine::load(&root.join(".fino/config.toml"), None)
            } else {
                Engine::load_fast(&root.join(".fino/config.toml"))
            }
            .unwrap();
            assert!(problems.iter().flat_map(|p| &p.errors).any(|error| error.contains("optimized wildcard-parent roots with children are unsupported; name the parent node kind")), "{problems:?}");
        }
    }
}

const DIALECT: &str = r#"
[[suppression_dialects]]
id = "review"
file = '#\s*review:\s*file\s+(?P<targets>.*)$'
next_line = '#\s*review:\s*next\s+(?P<targets>.*)$'
normalize_hyphens = true
advance_consecutive = true
"#;

fn aliased_debug() -> String {
    format!("{DEBUG}\n[compat.suppressions]\nreview = ['trace_call', 'old_trace']\n")
}

#[test]
fn external_dialects_target_aliases_and_preserve_native_and_credo() {
    let debug = format!("{}\n[compat]\ncredo = 'Example.Trace'\n", aliased_debug());
    let other = "id = 'other'\nmessage = 'm'\nregex = 'IO'\n";
    let config = format!("[compat]\ncredo_directives = true\n{DIALECT}");
    let (_, engine) = project(&config, &[&debug, other]);
    let text = "# review: next trace-call\nIO.inspect(1)\nIO.inspect(2)\n# review: next old-trace\nIO.inspect(3)\n";
    let rows = lint(&engine, "a.ex", text).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.rule == "debug")
            .map(|r| r.line)
            .collect::<Vec<_>>(),
        [3]
    );
    assert_eq!(rows.iter().filter(|r| r.rule == "other").count(), 3);
    for directive in [
        "# review: file old_trace",
        "# fino:disable-file debug",
        "# credo:disable-for-this-file Example.Trace",
    ] {
        let rows = lint(&engine, "a.ex", &format!("{directive}\nIO.inspect(1)\n")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule, "other");
    }
    // Neither an unknown target nor a rule id implicitly suppresses a rule.
    assert_eq!(
        lines(
            &engine,
            "a.ex",
            "# review: file debug unknown\nIO.inspect(1)\n"
        )
        .len(),
        2
    );
}

#[test]
fn external_dialect_chains_advance_only_across_recognized_adjacent_lines() {
    let (_, on) = project(DIALECT, &[&aliased_debug()]);
    let (_, off) = project(
        &DIALECT.replace("advance_consecutive = true", "advance_consecutive = false"),
        &[&aliased_debug()],
    );
    let text = "# review: next trace-call\n# review: next unrelated\nIO.inspect(1)\n# review: next trace_call\n# unrelated comment\nIO.inspect(2)\n# review: next trace_call\n\nIO.inspect(3)\n";
    assert_eq!(lines(&on, "a.ex", text), [6, 9]);
    assert_eq!(lines(&off, "a.ex", text), [3, 6, 9]);
    let long_chain = format!(
        "# review: next trace-call\n{}IO.inspect(1)\n",
        "# review: next unknown\n".repeat(1000)
    );
    assert!(lines(&on, "a.ex", &long_chain).is_empty());
    // Advancement is based on recognized directive lines, even with leading code.
    assert_eq!(
        lines(
            &on,
            "a.ex",
            "# review: next trace-call\nIO.inspect(1) # review: next unknown\n"
        ),
        [2]
    );
}

#[test]
fn external_dialects_are_comment_only_unless_explicitly_legacy_raw() {
    let (_, comments) = project(DIALECT, &[&aliased_debug()]);
    let (_, raw) = project(
        &format!("{DIALECT}\nraw_text = true\n"),
        &[&aliased_debug()],
    );
    let source = "value = \"\"\"\n# review: file trace-call\n\"\"\"\nIO.inspect(1)\n";
    assert_eq!(lines(&comments, "a.ex", source), [4]);
    assert!(lines(&raw, "a.ex", source).is_empty());
    assert!(
        lines(
            &comments,
            "a.ex",
            "IO.inspect(1) # review: file trace-call\n"
        )
        .is_empty()
    );
    let (_, no_normalization) = project(
        &DIALECT.replace("normalize_hyphens = true", "normalize_hyphens = false"),
        &[&aliased_debug()],
    );
    assert_eq!(
        lines(
            &no_normalization,
            "a.ex",
            "# review: file trace-call\nIO.inspect(1)\n"
        ),
        [2]
    );
}

#[test]
fn dialect_validation_and_malformed_targets_fail_closed() {
    for definition in [
        "id = 'review'",
        "id = 'BAD'\nfile = '(?P<targets>x)'",
        "id = 'review'\nfile = '['",
        "id = 'review'\nfile = 'no_capture'",
        "id = 'review'\nfile = '(?P<targets>.*)'",
    ] {
        let root = fixture();
        let config = root.join("config.toml");
        fs::write(&config, format!("[[suppression_dialects]]\n{definition}\n")).unwrap();
        assert!(Engine::load(&config, None).is_err(), "{definition}");
        assert!(Engine::load_fast(&config).is_err(), "{definition}");
    }
    let root = fixture();
    let config = root.join("config.toml");
    fs::write(&config, format!("{DIALECT}\n{DIALECT}")).unwrap();
    assert!(Engine::load(&config, None).is_err());
    let (_, engine) = project(DIALECT, &[&aliased_debug()]);
    assert!(lint(&engine, "a.ex", "# review: file \nIO.inspect(1)\n").is_err());
    // Optional captures that fail to participate are not treated as blanket suppressions.
    let optional_capture =
        "[[suppression_dialects]]\nid = 'review'\nfile = '# review(?P<targets> x)?'\n";
    let (_, engine) = project(optional_capture, &[&aliased_debug()]);
    assert!(lint(&engine, "a.ex", "# review\nIO.inspect(1)\n").is_err());

    let path = root.join("debug.toml");
    for alias in ["[]", "['']", "['two tokens']"] {
        fs::write(
            &path,
            format!("{DEBUG}\n[compat.suppressions]\nreview = {alias}\n"),
        )
        .unwrap();
        assert!(compile_rule(&path).0.is_none());
    }
    let rules = root.join("rules");
    fs::create_dir(&rules).unwrap();
    fs::write(rules.join("debug.toml"), aliased_debug()).unwrap();
    fs::write(&config, "").unwrap();
    for (_, problems) in [
        Engine::load(&config, None).unwrap(),
        Engine::load_fast(&config).unwrap(),
    ] {
        assert!(problems.iter().any(|p| {
            p.errors
                .iter()
                .any(|e| e.contains("unknown suppression dialect"))
        }));
    }
}

#[test]
fn regex_path_conditions_compose_with_globs_and_match_grouped_loading() {
    let rule = format!(
        "{DEBUG}\ninclude = ['lib/**/*.ex']\nexclude = ['lib/ignored']\nif_path = {{ regex = '(^|/)v[2-9]_[^/]+\\.ex$' }}\n[compat.suppressions]\nreview = ['trace_call']\n"
    );
    let (root, strict) = project(DIALECT, &[&rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    for (path, count) in [
        ("lib/v2_sample.ex", 1),
        ("lib/v1_sample.ex", 0),
        ("lib/ignored/v2_sample.ex", 0),
        ("other/v2_sample.ex", 0),
    ] {
        for text in [
            "IO.inspect(1)\n",
            "# review: next trace-call\nIO.inspect(1)\n",
        ] {
            let a = lint(&strict, path, text).unwrap();
            assert_eq!(a, lint(&fast, path, text).unwrap());
            assert_eq!(a.len(), if text.starts_with('#') { 0 } else { count });
        }
    }
    let path = root.join("invalid.toml");
    for value in ["{ regex = '[' }", "{ regex = '' }", "{}", "{ query = 'x' }"] {
        fs::write(
            &path,
            format!("id = 'invalid'\nmessage = 'm'\npath_only = true\nif_path = {value}\n"),
        )
        .unwrap();
        assert!(compile_rule(&path).0.is_none());
    }
    let rule = "id = 'path'\nmessage = 'm'\npath_only = true\nif_path = { regex = '^old/' }\n";
    let engine = engine(&[rule]);
    assert_eq!(lines(&engine, "old/a.ex", ":ok"), [1]);
    assert!(lines(&engine, "new/a.ex", ":ok").is_empty());
}

#[test]
fn auxiliary_count_predicates_still_apply_with_external_suppressions() {
    let rule = format!(
        "{}\nif_file = {{ query = '((list) @xs (#count-gt? @xs 1))' }}\nskip_lines = {{ query = '((list) @xs (#count-eq? @xs 1))' }}\n[compat.suppressions]\nreview = ['trace_call']\n",
        DEBUG
    );
    let (root, strict) = project(DIALECT, &[&rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    for (source, expected) in [
        ("IO.inspect([1])\n", vec![]),
        ("IO.inspect([1, 2])\nIO.inspect([3])\n", vec![1]),
        (
            "# review: next trace-call\nIO.inspect([1, 2])\nIO.inspect([3])\n",
            vec![],
        ),
    ] {
        assert_eq!(lines(&strict, "a.ex", source), expected);
        assert_eq!(
            lint(&strict, "a.ex", source).unwrap(),
            lint(&fast, "a.ex", source).unwrap()
        );
    }
}

#[test]
fn dialects_are_independent_and_can_suppress_multiple_rule_aliases() {
    let config = format!(
        "{DIALECT}\n[[suppression_dialects]]\nid = 'audit'\nnext_line = '# audit (?P<targets>.+)'\n"
    );
    let debug = format!("{}\naudit = ['trace_call']\n", aliased_debug());
    let other = "id = 'other'\nmessage = 'm'\nregex = 'IO'\n[compat.suppressions]\nreview = ['trace-call', 'another']\n";
    let (_, engine) = project(&config, &[&debug, other]);
    assert!(
        lines(
            &engine,
            "a.ex",
            "# review: next unknown trace_call\nIO.inspect(1)\n"
        )
        .is_empty()
    );
    let rows = lint(&engine, "a.ex", "# audit trace_call\nIO.inspect(1)\n").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rule, "other");
    // Audit targets do not inherit the other dialect's normalization option.
    assert_eq!(
        lines(&engine, "a.ex", "# audit trace-call\nIO.inspect(1)\n").len(),
        2
    );
}

#[test]
fn external_dialects_merge_resolved_targets_without_leaking_scope_or_normalization() {
    let config = format!(
        "{DIALECT}\n[[suppression_dialects]]\nid = 'audit'\nfile = '# audit: file (?P<targets>.+)'\nnext_line = '# audit: next (?P<targets>.+)'\nadvance_consecutive = true\n"
    );
    let debug = format!("{}\naudit = ['trace-call']\n", aliased_debug());
    let other = "id = 'other'\nmessage = 'm'\nregex = 'IO'\n[compat.suppressions]\nreview = ['second-call']\n";
    let (root, strict) = project(&config, &[&debug, other]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    for (source, expected) in [
        (
            "# review: next trace-call\n# review: next unknown\n# review: next second_call\nIO.inspect(1)\nIO.inspect(2)\n",
            vec![("debug", 5), ("other", 5)],
        ),
        (
            "# review: file unknown\n# review: file second_call\n# audit: file trace_call\nIO.inspect(1)\n",
            vec![("debug", 4)],
        ),
        (
            "# review: next second_call\n# audit: next trace-call\nIO.inspect(1)\n",
            vec![("other", 3)],
        ),
        (
            "# review: file unknown\n# review: file trace-call\n# review: file second_call\nIO.inspect(1)\n",
            vec![],
        ),
    ] {
        let rows = lint(&strict, "a.ex", source).unwrap();
        assert_eq!(rows, lint(&fast, "a.ex", source).unwrap());
        assert_eq!(
            rows.iter()
                .map(|row| (row.rule.as_str(), row.line))
                .collect::<Vec<_>>(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn legacy_raw_dialects_and_rule_examples_match_grouped_loading() {
    let rule = format!(
        r#"{}
[[tests]]
name = 'file alias'
code = '''# review: file trace-call
IO.inspect(1)'''
expect = []
[[tests]]
name = 'finding'
code = 'IO.inspect(1)'
expect = [1]
"#,
        aliased_debug()
    );
    let (root, strict) = project(&format!("{DIALECT}\nraw_text = true\n"), &[&rule]);
    let (fast, problems) = Engine::load_fast(&root.join(".fino/config.toml")).unwrap();
    assert!(problems.iter().all(|p| p.errors.is_empty()));
    let source = "value = \"\"\"\n# review: next trace-call\n\"\"\"\nIO.inspect(1)\n";
    assert_eq!(lines(&strict, "a.ex", source), [4]);
    assert_eq!(
        lint(&strict, "a.ex", source).unwrap(),
        lint(&fast, "a.ex", source).unwrap()
    );
    let source = "value = \"\"\"\n# review: file trace-call\n\"\"\"\nIO.inspect(1)\n";
    assert!(lines(&strict, "a.ex", source).is_empty());
    assert_eq!(
        lint(&strict, "a.ex", source).unwrap(),
        lint(&fast, "a.ex", source).unwrap()
    );
    assert!(run_tests(&strict).iter().all(|outcome| outcome.passed));
}
