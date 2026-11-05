//! Parallel, syntax-aware Elixir linting driven by declarative rule files.
//! No application code is evaluated.
mod directives;
mod engine;
mod mask;
pub mod model;
mod scan;
mod snapshot;
mod testing;
mod tree;

pub use engine::{Engine, Problems, Rule, compile_rule, load_config, rule_files};
pub use model::Severity;
use serde::Serialize;
pub use snapshot::{Entry, Snapshot};
pub use testing::{TestOutcome, run_tests};
pub use tree::render as render_tree;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub rule: String,
    pub message: String,
    pub severity: Severity,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub files: usize,
    pub findings: Vec<Finding>,
    pub errors: Vec<Diagnostic>,
    pub current: std::collections::BTreeSet<Entry>,
    pub new: Vec<Entry>,
    pub stale: Vec<Entry>,
    pub scope: String,
    pub elapsed_ms: f64,
    pub passed: bool,
}

impl Report {
    pub fn exit_code(&self) -> i32 {
        if !self.errors.is_empty() {
            2
        } else if self.passed {
            0
        } else {
            1
        }
    }
}

impl Engine {
    /// Scan a complete root, or explicit relative Elixir paths.
    /// Explicit paths bypass gitignore, but not configured path filters.
    pub fn scan(
        &self,
        root: &std::path::Path,
        files: Option<&[String]>,
        threads: usize,
        snapshot: &Snapshot,
    ) -> Result<Report> {
        scan::run(self, root, files, threads, snapshot)
    }
}

#[cfg(test)]
mod tests;
