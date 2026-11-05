use std::process::Command;

fn fino(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fino"))
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap()
}

#[test]
fn skills_are_discoverable_and_embedded_without_loading_project_config() {
    let output = fino(&["--help"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("skills"));
    let output = fino(&["skills", "--config", "does-not-exist.toml"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("fino-rules") && text.contains("fino-config"));
    assert!(text.contains("aubx skills add"));

    for (topic, content) in [
        ("rules", include_str!("../skills/fino-rules/SKILL.md")),
        ("config", include_str!("../skills/fino-config/SKILL.md")),
    ] {
        let output = fino(&["skills", topic]);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        assert_eq!(output.stdout, content.as_bytes());
        let output = fino(&["skills", topic, "--json"]);
        assert!(output.status.success(), "{output:?}");
        let skills: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(skills.as_array().unwrap().len(), 1);
        assert_eq!(skills[0]["topic"], topic);
        assert_eq!(skills[0]["content"], content);
    }

    let output = fino(&["skills", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let skills: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(skills.as_array().unwrap().len(), 2);

    for args in [vec!["skills", "unknown"], vec!["skills", "rules", "config"]] {
        let output = fino(&args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
    }

    let output = fino(&["--usage-spec"]);
    let spec: clap_usage::usage::Spec = String::from_utf8(output.stdout).unwrap().parse().unwrap();
    assert!(spec.cmd.subcommands.contains_key("skills"));
}

#[test]
fn documented_example_policy_validates_and_all_embedded_examples_pass() {
    for command in ["validate", "test"] {
        let output = fino(&[command, "--root", "examples/rules"]);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}
