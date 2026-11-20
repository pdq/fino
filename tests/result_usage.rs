use fino::{Engine, Snapshot};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

fn project(source: &str) -> (PathBuf, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-fixtures")
        .join(format!(
            "result-usage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(root.join(".fino/rules")).unwrap();
    let config = root.join(".fino/config.toml");
    fs::write(&config, "").unwrap();
    fs::write(root.join(".fino/rules/discarded.toml"),
        "id = 'discarded'\nmessage = 'review result'\npattern = 'Sample.transform(...)'\nreport = 'callee'\nresult = 'discarded'\n").unwrap();
    fs::write(root.join("sample.ex"), source).unwrap();
    (root, config)
}

fn check(source: &str, lines: &[usize]) {
    let (root, config) = project(source);
    for fast in [false, true] {
        let (engine, problems) = if fast {
            Engine::load_fast(&config)
        } else {
            Engine::load(&config, None)
        }
        .unwrap();
        assert!(problems.iter().all(|p| p.errors.is_empty()), "{problems:?}");
        let report = engine
            .scan(&root, Some(&["sample.ex".into()]), 1, &Snapshot::default())
            .unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.findings.iter().map(|f| f.line).collect::<Vec<_>>(),
            lines,
            "{source}"
        );
        for finding in report.findings {
            assert_eq!(&source[finding.start_byte..finding.end_byte], "transform");
        }
    }
}

#[test]
fn pipes_and_nested_calls_classify_the_expression_not_the_report_token() {
    check(
        "def run(xs) do\n\
 Sample.transform(xs)\n\
 xs |> Sample.transform()\n\
 Sample.transform(xs) |> consume()\n\
 consume(Sample.transform(xs))\n\
 _ = Sample.transform(xs)\n\
 Sample.transform(xs)\n\
end",
        &[2, 3],
    );
}

#[test]
fn branches_and_function_values_have_independent_returns() {
    check(
        "def run(xs) do\n\
 if ready?, do: Sample.transform(xs), else: :ok\n\
 fn -> Sample.transform(xs) end\n\
 fn -> Sample.transform(xs); :ok end\n\
 case xs do\n\
   [] -> Sample.transform(xs)\n\
   _ -> :ok\n\
 end\n\
 :ok\n\
end",
        &[2, 4, 6],
    );
}

#[test]
fn try_cleanup_is_discarded_but_receive_timeout_branch_is_returned() {
    check(
        "def run(xs) do\n\
 try do\n\
  :ok\n\
 after\n\
  Sample.transform(xs)\n\
 end\n\
end\n\
def receive_value(xs) do\n\
 receive do\n\
   :ready -> :ok\n\
 after\n\
   10 -> Sample.transform(xs)\n\
 end\n\
end",
        &[5],
    );
}

#[test]
fn quotes_unknown_macros_and_module_bodies_are_not_execution_proofs() {
    check(
        "Sample.transform(xs)\n\
def run(xs) do\n\
 custom do\n\
  Sample.transform(xs)\n\
  :ok\n\
 end\n\
 quote do\n\
  def nested(xs) do\n\
   Sample.transform(xs)\n\
   :ok\n\
  end\n\
 end\n\
 :ok\n\
end",
        &[],
    );
}

#[test]
fn result_option_cannot_silently_apply_to_other_matchers() {
    let (root, config) = project(":ok");
    fs::write(
        root.join(".fino/rules/discarded.toml"),
        "id = 'discarded'\nmessage = 'm'\nquery = '(call) @finding'\nresult = 'discarded'\n",
    )
    .unwrap();
    for fast in [false, true] {
        let (_, problems) = if fast {
            Engine::load_fast(&config)
        } else {
            Engine::load(&config, None)
        }
        .unwrap();
        assert!(
            problems
                .iter()
                .flat_map(|p| &p.errors)
                .any(|e| e.contains("result only applies to pattern"))
        );
    }
}

#[test]
fn filtering_one_rule_does_not_change_another_rules_result_contract() {
    let source = "def run(xs) do\n Sample.transform(xs)\n Sample.transform(xs)\nend";
    let (root, config) = project(source);
    fs::write(
        root.join(".fino/rules/all.toml"),
        "id = 'all'\nmessage = 'm'\npattern = 'Sample.transform(...)'\nreport = 'callee'\n",
    )
    .unwrap();
    let (engine, _) = Engine::load_fast(&config).unwrap();
    let report = engine
        .scan(&root, Some(&["sample.ex".into()]), 1, &Snapshot::default())
        .unwrap();
    assert!(report.errors.is_empty());
    assert_eq!(
        report
            .findings
            .iter()
            .map(|f| (f.rule.as_str(), f.line))
            .collect::<Vec<_>>(),
        [("all", 2), ("discarded", 2), ("all", 3)]
    );
}

#[test]
fn ancestry_exhaustion_is_a_scan_error_not_a_clean_result() {
    let nested = format!("{}Sample.transform(xs){}", "(".repeat(300), ")".repeat(300));
    let (root, config) = project(&format!("def run(xs) do\n {nested}\n :ok\nend"));
    for fast in [false, true] {
        let (engine, _) = if fast {
            Engine::load_fast(&config)
        } else {
            Engine::load(&config, None)
        }
        .unwrap();
        let report = engine
            .scan(&root, Some(&["sample.ex".into()]), 1, &Snapshot::default())
            .unwrap();
        assert!(report.findings.is_empty());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("result position nesting limit")),
            "{:?}",
            report.errors
        );
    }
}

#[test]
fn wide_branch_sets_do_not_rescan_other_alternatives() {
    let branches = (0..2048)
        .map(|n| format!(" {n} -> Sample.transform(xs)\n"))
        .collect::<String>();
    let source = format!("def run(xs) do\n case xs do\n{branches} end\n :ok\nend");
    check(&source, &(3..2051).collect::<Vec<_>>());
}

#[test]
fn sequences_in_unknown_macro_arguments_are_not_proven_executed() {
    check(
        "def run(xs) do\n\
 custom((Sample.transform(xs); :ok))\n\
 consume(fn -> Sample.transform(xs); :ok end)\n\
 if (Sample.transform(xs); :ok), do: :yes\n\
 :ok\nend",
        &[3, 4],
    );
}

#[test]
fn inactive_same_signature_routes_still_have_bounded_work() {
    let source = format!(
        "def run(xs) do\n{} :ok\nend",
        " Sample.transform(xs)\n".repeat(5000)
    );
    let (root, config) = project(&source);
    for n in 0..512 {
        fs::write(root.join(format!(".fino/rules/excluded-{n}.toml")),
            format!("id = 'excluded-{n}'\nmessage = 'm'\npattern = 'Sample.transform(...)'\ninclude = ['other.ex']\n")).unwrap();
    }
    for fast in [false, true] {
        let (engine, _) = if fast {
            Engine::load_fast(&config)
        } else {
            Engine::load(&config, None)
        }
        .unwrap();
        let report = engine
            .scan(&root, Some(&["sample.ex".into()]), 1, &Snapshot::default())
            .unwrap();
        assert!(report.findings.is_empty());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("pattern work limit")),
            "{:?}",
            report.errors
        );
    }
}
