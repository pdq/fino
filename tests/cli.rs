use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/cli-tests")
            .join(format!("{}-{stamp}", std::process::id()));
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
    project.source("IO.inspect(:old)\n");
    project.run("check", &[], 1);
    project.run("snapshot", &[], 0);
    let original = project.policy();
    project.source("\n\n  IO.inspect(:old)\n");
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
