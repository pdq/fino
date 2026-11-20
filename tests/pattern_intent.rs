use fino::{Engine, Finding, Snapshot};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

fn check(source: &str, expected: &[(&str, usize, &str)]) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-fixtures")
        .join(format!(
            "pattern-intent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    let rules = root.join(".fino/rules");
    fs::create_dir_all(&rules).unwrap();
    fs::write(root.join(".fino/config.toml"), "").unwrap();
    for (name, contents) in [
        (
            "identity-map",
            include_str!("../examples/patterns/.fino/rules/identity-map.toml"),
        ),
        (
            "filter-nil",
            include_str!("../examples/patterns/.fino/rules/filter-nil.toml"),
        ),
        (
            "case-true-false",
            include_str!("../examples/patterns/.fino/rules/case-true-false.toml"),
        ),
    ] {
        fs::write(rules.join(format!("{name}.toml")), contents).unwrap();
    }
    fs::write(root.join("sample.ex"), source).unwrap();
    let config = root.join(".fino/config.toml");
    let (strict, strict_problems) = Engine::load(&config, None).unwrap();
    let (grouped, grouped_problems) = Engine::load_fast(&config).unwrap();
    assert!(
        strict_problems.iter().all(|p| p.errors.is_empty()),
        "{strict_problems:?}"
    );
    assert!(
        grouped_problems.iter().all(|p| p.errors.is_empty()),
        "{grouped_problems:?}"
    );
    let scan = |engine: &Engine| {
        let report = engine
            .scan(&root, Some(&["sample.ex".into()]), 1, &Snapshot::default())
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report.findings
    };
    let findings: Vec<Finding> = scan(&strict);
    assert_eq!(findings, scan(&grouped), "strict and grouped loads differ");
    let mut actual: Vec<_> = findings
        .iter()
        .map(|f| {
            let token = &source[f.start_byte..f.end_byte];
            let line_start = source[..f.start_byte].rfind('\n').map_or(0, |i| i + 1);
            assert_eq!(f.column, f.start_byte - line_start + 1);
            (f.rule.as_str(), f.line, token)
        })
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn identity_requires_one_variable_and_one_expression() {
    check(
        "Enum.map(xs, fn x -> x end)\n\
         Enum.map(xs, fn x, y -> x end)\n\
         Enum.map(xs, fn x -> y end)\n\
         Enum.map(xs, fn {x} -> x end)\n\
         Enum.map(xs, fn x -> x; log(x) end)\n\
         Enum.map(xs, fn x -> x; y -> y end)\n",
        &[("identity-map", 1, "map")],
    );
}

#[test]
fn filter_requires_same_variable_and_single_clause() {
    check(
        "Enum.filter(xs, fn x -> x != nil end)\n\
         Enum.filter(xs, fn x -> nil !== x end)\n\
         Enum.filter(xs, fn x -> not is_nil(x) end)\n\
         Enum.filter(xs, fn x -> y != nil end)\n\
         Enum.filter(xs, fn x, y -> x != nil end)\n\
         Enum.filter(xs, fn x -> x != nil; log(x) end)\n\
         Enum.filter(xs, fn nil -> false; x -> x != nil end)\n",
        &[
            ("filter-nil", 1, "filter"),
            ("filter-nil", 2, "filter"),
            ("filter-nil", 3, "filter"),
        ],
    );
}

#[test]
fn nested_callbacks_do_not_leak_bindings_between_matches() {
    check(
        "Enum.map(Enum.map(xs, fn inner -> inner end), fn outer -> outer end)\n\
         Enum.map(Enum.map(xs, fn inner -> other end), fn outer -> outer end)\n\
         Enum.map(Enum.map(xs, fn inner -> inner end), fn outer -> inner end)\n",
        &[
            ("identity-map", 1, "map"),
            ("identity-map", 1, "map"),
            ("identity-map", 2, "map"),
            ("identity-map", 3, "map"),
        ],
    );
}

#[test]
fn pipes_insert_one_argument_and_preserve_quoted_callee_tokens() {
    check(
        "xs |> Enum.map(fn x -> x end)\n\
         xs |> Enum.map(extra, fn x -> x end)\n\
         Enum.\"m\\x61p\"(xs, fn x -> x end)\n\
         xs |> Enum.\"f\\u0069lter\"(fn x -> !is_nil(x) end)\n\
         xs |> Enum.filter(extra, fn x -> x != nil end)\n\
         Enum.\"mapper\"(xs, fn x -> x end)\n",
        &[
            ("identity-map", 1, "map"),
            ("identity-map", 3, "\"m\\x61p\""),
            ("filter-nil", 4, "\"f\\u0069lter\""),
        ],
    );
}

#[test]
fn boolean_case_requires_exactly_two_boolean_clauses() {
    check(
        "case ready? do true -> :yes; false -> :no end\n\
         case ready? do false -> :no; true -> :yes end\n\
         case ready? do true -> :yes; false -> :no; nil -> :unknown end\n\
         case ready? do true -> :yes; _ -> :no end\n\
         case ready? do true -> :yes; true -> :again end\n",
        &[
            ("case-true-false", 1, "case"),
            ("case-true-false", 2, "case"),
        ],
    );
}

fn check_policy(
    rules: &[(&str, &str)],
    path: &str,
    source: &str,
    expected: &[(&str, usize, &str)],
) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-fixtures")
        .join(format!(
            "pattern-routing-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    let rule_dir = root.join(".fino/rules");
    fs::create_dir_all(&rule_dir).unwrap();
    fs::write(root.join(".fino/config.toml"), "").unwrap();
    for (name, definition) in rules {
        fs::write(rule_dir.join(format!("{name}.toml")), definition).unwrap();
    }
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, source).unwrap();
    let config = root.join(".fino/config.toml");
    let (strict, strict_problems) = Engine::load(&config, None).unwrap();
    let (grouped, grouped_problems) = Engine::load_fast(&config).unwrap();
    for problems in [&strict_problems, &grouped_problems] {
        assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
    }
    let scan = |engine: &Engine| {
        let report = engine
            .scan(&root, Some(&[path.to_owned()]), 1, &Snapshot::default())
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report.findings
    };
    let findings = scan(&strict);
    assert_eq!(findings, scan(&grouped), "strict and grouped loads differ");
    let mut actual: Vec<_> = findings
        .iter()
        .map(|f| {
            let token = &source[f.start_byte..f.end_byte];
            let line_start = source[..f.start_byte].rfind('\n').map_or(0, |i| i + 1);
            assert_eq!(f.column, f.start_byte - line_start + 1);
            (f.rule.as_str(), f.start_byte, token)
        })
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn raw_query_and_pattern_both_report_same_call_and_distinct_occurrences() {
    let source = "IO.inspect(first); IO.inspect(second)\n";
    let positions: Vec<_> = source.match_indices("inspect").map(|(i, _)| i).collect();
    check_policy(
        &[
            (
                "raw-inspect",
                r#"id = "raw-inspect"
message = "raw"
query = '''((dot left: (alias) @_module right: (identifier) @finding)
  (#eq? @_module "IO") (#eq? @finding "inspect"))'''
"#,
            ),
            (
                "pattern-inspect",
                r#"id = "pattern-inspect"
message = "pattern"
pattern = "IO.inspect(_)"
report = "callee"
"#,
            ),
        ],
        "sample.ex",
        source,
        &[
            ("raw-inspect", positions[0], "inspect"),
            ("raw-inspect", positions[1], "inspect"),
            ("pattern-inspect", positions[0], "inspect"),
            ("pattern-inspect", positions[1], "inspect"),
        ],
    );
}

#[test]
fn shared_callee_routes_by_module_and_call_kind() {
    let source =
        "Enum.map(xs, fun); Stream.\"m\\x61p\"(xs, fun); map(xs, fun); Other.map(xs, fun)\n";
    check_policy(
        &[
            (
                "enum-map",
                "id = 'enum-map'\nmessage = 'enum'\npattern = 'Enum.map(_, _)'\nreport = 'callee'\n",
            ),
            (
                "stream-map",
                "id = 'stream-map'\nmessage = 'stream'\npattern = 'Stream.map(_, _)'\nreport = 'callee'\n",
            ),
            (
                "local-map",
                "id = 'local-map'\nmessage = 'local'\npattern = 'map(_, _)'\nreport = 'callee'\n",
            ),
        ],
        "sample.ex",
        source,
        &[
            ("enum-map", source.find("map").unwrap(), "map"),
            (
                "stream-map",
                source.find("\"m\\x61p\"").unwrap(),
                "\"m\\x61p\"",
            ),
            (
                "local-map",
                source.find("map(xs, fun); Other").unwrap(),
                "map",
            ),
        ],
    );
}

#[test]
fn filtering_and_suppression_one_rule_leave_shared_candidates_available() {
    let source = "# fino:disable-next-line filtered-map\nEnum.map(xs, fun)\nEnum.map(ys, fun)\n";
    let positions: Vec<_> = source.match_indices("map(").map(|(i, _)| i).collect();
    let rules = [
        (
            "filtered-map",
            "id = 'filtered-map'\nmessage = 'filtered'\npattern = 'Enum.map(_, _)'\nreport = 'callee'\ninclude = ['lib/**']\n",
        ),
        (
            "other-map",
            "id = 'other-map'\nmessage = 'other'\npattern = 'Enum.map(_, _)'\nreport = 'callee'\n",
        ),
    ];
    check_policy(
        &rules,
        "lib/sample.ex",
        source,
        &[
            ("other-map", positions[0], "map"),
            ("other-map", positions[1], "map"),
            ("filtered-map", positions[1], "map"),
        ],
    );
    check_policy(
        &rules,
        "other/sample.ex",
        source,
        &[
            ("other-map", positions[0], "map"),
            ("other-map", positions[1], "map"),
        ],
    );
}
