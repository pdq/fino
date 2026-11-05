//! Runs the `[[tests]]` examples embedded in rule files.
use crate::{Engine, scan};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TestOutcome {
    pub rule: String,
    pub file: String,
    pub name: String,
    pub passed: bool,
    pub expected: Vec<usize>,
    pub actual: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Run every loaded rule's examples. Each example is linted with only its own rule;
/// rule path filters apply, project-level `include`/`exclude` do not.
pub fn run_tests(engine: &Engine) -> Vec<TestOutcome> {
    let (mut parser, mut cursor) = scan::worker();
    let mut outcomes = Vec::new();
    for rule in &engine.rules {
        for test in &rule.tests {
            let mut expected = test.expect.clone();
            expected.sort_unstable();
            let result = scan::source(
                engine,
                Some(rule),
                &mut parser,
                &mut cursor,
                &test.path,
                &test.code,
            );
            let (actual, error) = match result {
                Ok(findings) => {
                    let mut lines: Vec<usize> = findings.iter().map(|f| f.line).collect();
                    lines.sort_unstable();
                    (lines, None)
                }
                Err(d) => (
                    Vec::new(),
                    Some(format!("{}:{}: {}", d.line, d.column, d.message)),
                ),
            };
            outcomes.push(TestOutcome {
                rule: rule.id.clone(),
                file: rule.file.display().to_string(),
                name: test.name.clone(),
                passed: error.is_none() && actual == expected,
                expected,
                actual,
                error,
            });
        }
    }
    outcomes
}
