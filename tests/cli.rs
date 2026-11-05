use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static PROJECT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn fino(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fino"))
        .args(args)
        .output()
        .unwrap()
}

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/cli-tests")
            .join(format!(
                "{}-{stamp}-{}",
                std::process::id(),
                PROJECT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
        let rules = root.join(".fino/rules");
        fs::create_dir_all(&rules).unwrap();
        fs::write(root.join(".fino/config.toml"), "").unwrap();
        fs::write(
            rules.join("no-inspect.toml"),
            "id = 'no-inspect'\nmessage = 'Remove debug'\nregex = 'IO\\.inspect'\n",
        )
        .unwrap();
        Self(root)
    }

    fn source(&self, text: &str) {
        fs::write(self.0.join("example.ex"), text).unwrap();
    }

    fn policy(&self) -> Vec<u8> {
        fs::read(self.0.join(".fino/snapshot.jsonl")).unwrap()
    }

    fn run(&self, command: &str, args: &[&str], exit: i32) -> serde_json::Value {
        let output = Command::new(env!("CARGO_BIN_EXE_fino"))
            .args([command, "--json", "--threads", "2", "--root"])
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(exit), "{output:?}");
        if output.stdout.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&output.stdout).unwrap()
        }
    }
}

#[test]
fn snapshot_cli_adopts_explicitly_and_updates_without_accepting_new_debt() {
    let project = Project::new();
    let original_source = "IO.inspect(:old)\n";
    project.source(original_source);
    project.run("check", &[], 1);
    project.run("snapshot", &[], 0);
    let original = project.policy();
    project.source(&format!("\n\n  {original_source}"));
    project.run("check", &["example.ex", "README.md"], 0);
    assert_eq!(project.policy(), original);

    project.source("IO.inspect(:new)\n");
    let report = project.run("check", &[], 1);
    assert_eq!(report["new"].as_array().unwrap().len(), 1);
    assert_eq!(report["stale"].as_array().unwrap().len(), 1);
    assert_eq!(project.policy(), original);
    project.run("snapshot", &["--update", "example.ex"], 1);
    assert!(project.policy().is_empty());
    project.run("check", &[], 1);

    project.run("snapshot", &[], 0);
    let original = project.policy();
    project.source("defmodule Broken do\n");
    project.run("snapshot", &["--update"], 2);
    assert_eq!(project.policy(), original);

    project.source(":ok\n");
    let report = project.run("check", &[], 0);
    assert_eq!(report["stale"].as_array().unwrap().len(), 1);
    assert_eq!(project.policy(), original);
    project.run("snapshot", &["--update"], 0);
    assert!(project.policy().is_empty());
    project.run("check", &["README.md"], 0);
    project.run("check", &["missing.ex"], 2);
}

#[test]
fn usage_spec_describes_commands_and_options_without_loading_a_project() {
    let output = fino(&["--usage-spec"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    let spec: clap_usage::usage::Spec = text.parse().unwrap();
    assert_eq!(spec.bin, "fino");
    for command in ["check", "snapshot", "validate", "test", "tree", "schema"] {
        assert!(spec.cmd.subcommands.contains_key(command), "{text}");
    }
    for flag in [
        "--root",
        "--config",
        "--threads",
        "--files-from",
        "--code",
        "--json",
        "--timings",
        "--update",
        "--color",
        "--context",
        "--group-by",
        "--help",
    ] {
        assert!(text.contains(flag), "{text}");
    }
    assert!(text.contains("rule") && text.contains("config"));
}

#[test]
fn help_version_and_argument_errors_keep_exit_codes() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["check", "--help"],
        vec!["--version"],
        vec!["-V"],
    ] {
        let output = fino(&args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    for args in [
        vec!["unknown"],
        vec!["check", "--unknown"],
        vec!["check", "--threads", "bad"],
        vec!["check", "--color", "bad"],
        vec!["check", "--context", "-1"],
        vec!["check", "--group-by", "file"],
        vec!["check", "--root"],
        vec!["tree"],
        vec!["schema", "other"],
    ] {
        let output = fino(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn file_arguments_can_surround_options_and_combine_with_a_manifest() {
    let project = Project::new();
    project.source("IO.inspect(:value)\n");
    fs::write(project.0.join("other.ex"), ":ok\n").unwrap();
    let manifest = project.0.join("files.txt");
    fs::write(&manifest, "other.ex\n\n").unwrap();
    let config = project.0.join(".fino/config.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_fino"))
        .args(["check", "example.ex", "--timings", "--root"])
        .arg(&project.0)
        .args(["--files-from"])
        .arg(manifest)
        .args(["--config"])
        .arg(config)
        .args(["README.md", "--json", "--threads", "1", "--threads", "2"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["files"], 2);
    assert_eq!(report["findings"].as_array().unwrap().len(), 1);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cumulative time across threads:")
    );
}

#[test]
fn validate_and_test_accept_selected_rule_files() {
    let project = Project::new();
    fs::write(project.0.join(".fino/rules/broken.toml"), "not a rule").unwrap();
    let selected = project.0.join(".fino/rules/no-inspect.toml");
    for command in ["validate", "test"] {
        let output = Command::new(env!("CARGO_BIN_EXE_fino"))
            .arg(command)
            .arg(&selected)
            .arg("--root")
            .arg(&project.0)
            .arg("--json")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(rows.is_array());
        project.run(command, &[], 2);
    }
}

#[test]
fn tree_accepts_code_a_file_and_stdin_and_schema_stays_json() {
    use std::io::Write;
    use std::process::Stdio;
    let project = Project::new();
    project.source("IO.inspect(:value)\n");
    let code = "IO.inspect(:value)\n";
    let from_code = fino(&["tree", "--code", code]);
    assert!(from_code.status.success(), "{from_code:?}");
    let from_file = fino(&["tree", project.0.join("example.ex").to_str().unwrap()]);
    assert!(from_file.status.success(), "{from_file:?}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_fino"))
        .args(["tree", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(code.as_bytes())
        .unwrap();
    let from_stdin = child.wait_with_output().unwrap();
    assert!(from_stdin.status.success());
    assert_eq!(from_code.stdout, from_file.stdout);
    assert_eq!(from_code.stdout, from_stdin.stdout);
    for kind in ["rule", "config"] {
        let output = fino(&["schema", kind]);
        assert!(output.status.success(), "{output:?}");
        let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(schema["properties"].is_object());
    }
}

impl Project {
    fn human(&self, args: &[&str], no_color: bool) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fino"));
        command
            .args(["check", "--root"])
            .arg(&self.0)
            .args(args)
            .env_remove("NO_COLOR");
        if no_color {
            command.env("NO_COLOR", "1");
        }
        command.output().unwrap()
    }
}

#[test]
fn human_colors_context_and_grouping_are_optional() {
    let project = Project::new();
    project.source("# before\nIO.inspect(:one)\nIO.inspect(:two)\n# after\n");
    let plain = project.human(&[], false);
    assert_eq!(plain.status.code(), Some(1));
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(!plain.contains('\x1b'));
    assert!(
        plain.contains("1 files, 1 rules, 2 findings, 0 errors"),
        "{plain}"
    );
    assert!(plain.contains("example.ex:2:1: error [no-inspect]: Remove debug"));
    assert!(
        plain.contains("1 | # before") && plain.contains("^^^^") && plain.contains("4 | # after")
    );
    assert!(!plain.contains("no-inspect —"));
    for (args, no_color, expected_color) in [
        (vec!["--color", "always"], false, true),
        (vec!["--color", "always"], true, true),
        (vec!["--color", "never"], false, false),
        (vec!["--color", "auto"], true, false),
    ] {
        let output = project.human(&args, no_color);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout.contains(&0x1b), expected_color);
    }
    let compact = project.human(&["--context", "0", "--group-by", "none"], false);
    let compact = String::from_utf8(compact.stdout).unwrap();
    assert!(!compact.contains(" | ") && !compact.contains(" — "));
    let grouped = project.human(&["--group-by", "rule", "--context", "0"], false);
    let grouped = String::from_utf8(grouped.stdout).unwrap();
    assert_eq!(grouped.matches("no-inspect — Remove debug").count(), 1);
    let rule = project.0.join(".fino/rules/no-inspect.toml");
    let text = fs::read_to_string(&rule).unwrap();
    fs::write(
        rule,
        format!("{text}description = 'Use structured logging instead.'\n"),
    )
    .unwrap();
    let grouped = project.human(&["--group-by", "rule"], false);
    let grouped = String::from_utf8(grouped.stdout).unwrap();
    assert_eq!(
        grouped.matches("Use structured logging instead.").count(),
        1
    );
}

#[test]
fn human_summary_counts_loaded_rules_including_path_filtered_rules() {
    let project = Project::new();
    project.source("IO.inspect(:value)\n");
    fs::write(
        project.0.join(".fino/rules/no-output.toml"),
        "id = 'no-output'\nmessage = 'Use logging'\nregex = 'IO\\.puts'\n",
    )
    .unwrap();
    fs::write(
        project.0.join(".fino/rules/review-generated.toml"),
        "id = 'review-generated'\nmessage = 'Review generated file'\npath_only = true\ninclude = ['generated/**']\n",
    )
    .unwrap();
    for group in ["none", "rule"] {
        let output = project.human(
            &["--color", "never", "--context", "0", "--group-by", group],
            false,
        );
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("FAIL: 1 files, 3 rules, 1 findings, 0 errors"),
            "{text}"
        );
    }
    project.source(":ok\n");
    let output = project.human(&["--color", "never"], false);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("PASS: 1 files, 3 rules, 0 findings, 0 errors"),
        "{text}"
    );
    let json = project.run("check", &[], 0);
    assert!(json.get("rules").is_none());
}

#[test]
fn human_options_never_change_json_shape_or_lint_truth() {
    let project = Project::new();
    project.source("IO.inspect(:value)\n");
    let mut baseline = project.run("check", &[], 1);
    let mut styled = project.run(
        "check",
        &["--color", "always", "--context", "9", "--group-by", "rule"],
        1,
    );
    baseline.as_object_mut().unwrap().remove("elapsed_ms");
    styled.as_object_mut().unwrap().remove("elapsed_ms");
    assert_eq!(baseline, styled);
    let missing = project.human(&["missing.ex", "--color", "never"], false);
    assert_eq!(missing.status.code(), Some(2));
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("missing.ex")
    );
}

#[test]
fn multiline_unicode_tabs_and_controls_have_safe_aligned_snippets() {
    let project = Project::new();
    fs::write(
        project.0.join(".fino/rules/no-inspect.toml"),
        "id = 'no-inspect'\nmessage = \"Remove\\u001b[31m debug\"\nquery = '(call) @finding'\n",
    )
    .unwrap();
    project.source("# café 界\n\tIO.inspect(\n\t:two)\n# after\n");
    let output = project.human(&["--color", "never"], false);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains('\x1b') && !text.contains('\t'));
    assert!(text.contains("Remove\\u{1b}[31m debug"), "{text}");
    assert!(text.contains("café 界"), "{text}");
    assert!(
        text.contains("2 |     IO.inspect") && text.contains("3 |     :two"),
        "{text}"
    );
    assert!(
        text.lines().filter(|line| line.contains('^')).count() >= 2,
        "{text}"
    );
}

#[test]
fn unicode_prefixes_remain_readable_with_cell_aligned_cli_carets() {
    let project = Project::new();
    for (prefix, cells) in [("é", 1), ("界", 2), ("e\u{301}", 1), ("👩\u{200d}💻", 2)] {
        project.source(&format!("\"{prefix}\"; IO.inspect(:value)\n"));
        let output = project.human(&["--color", "never"], false);
        assert_eq!(output.status.code(), Some(1));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains(&format!("\"{prefix}\"; IO.inspect")),
            "{text}"
        );
        let marker = text.lines().find(|line| line.contains('^')).unwrap();
        assert_eq!(
            marker,
            format!("    | {}^^^^^^^^^^", " ".repeat(cells + 4)),
            "{text}"
        );
    }
}

#[test]
fn human_warning_snapshot_and_validation_errors_remain_readable() {
    let project = Project::new();
    project.source("IO.inspect(:old)\n");
    project.run("snapshot", &[], 0);
    project.source(":ok\n");
    let output = project.human(&["--color", "never"], false);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("stale snapshot entry (run fino snapshot --update)")
    );
    let rule = project.0.join(".fino/rules/no-inspect.toml");
    let text = fs::read_to_string(&rule).unwrap();
    fs::write(&rule, format!("{text}severity = 'warning'\n")).unwrap();
    project.source("IO.inspect(:warning)\n");
    let output = project.human(&["--color", "never", "--group-by", "rule"], false);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("warning [no-inspect]")
    );
    fs::write(rule, "not a rule").unwrap();
    let output = project.human(&["--color", "never"], false);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains(": error:")
    );
}
