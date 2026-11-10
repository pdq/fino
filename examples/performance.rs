//! Opt-in synthetic wall-time measurements. See docs/performance.md.
use fino::{Engine, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const LEGACY_SCENARIOS: [&str; 3] = ["ast", "grouped", "regex-mask"];
const SCENARIOS: [&str; 6] = [
    "ast",
    "grouped",
    "regex-mask",
    "root-stage",
    "scoped-condition",
    "grapheme-length",
];
const UNIT: &str = "  def item() do\n    note = \"perf_marker\"\n    # perf_marker\n    perf_marker(note)\n  end\n";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    version: u32,
    file_counts: Vec<usize>,
    units_per_file: usize,
    samples: usize,
    warmups: usize,
    parallel_threads: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    relative_percent: f64,
    absolute_ms: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Budgets {
    version: u32,
    startup: Budget,
    evaluation: Budget,
    compilation: Budget,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Identity {
    runner: String,
    profile: String,
    os: String,
    arch: String,
    available_threads: usize,
    parallel_threads: usize,
    build: String,
    toolchain: String,
    corpus_hash: String,
    fino_version: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Metric {
    samples_ms: Vec<f64>,
    median_ms: f64,
    mad_ms: f64,
    min_ms: f64,
    max_ms: f64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Measurement {
    schema_version: u32,
    identity: Identity,
    revision: String,
    binary_hash: String,
    harness_binary_hash: String,
    harness_source_hash: String,
    linked_fino_version: String,
    captured_unix_seconds: u64,
    fixtures: Fixtures,
    metrics: BTreeMap<String, Metric>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

// Stable content fingerprint, not a cryptographic provenance/signature claim.
fn hash(bytes: &[u8]) -> String {
    let value = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    format!("fnv1a64:{value:016x}")
}

fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        values[middle - 1] / 2.0 + values[middle] / 2.0
    } else {
        values[middle]
    }
}

impl Metric {
    fn new(samples_ms: Vec<f64>) -> Result<Self> {
        if samples_ms.len() < 3 || samples_ms.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err("need at least three finite, nonnegative time samples".into());
        }
        let median_ms = median(&samples_ms);
        let deviations: Vec<_> = samples_ms.iter().map(|v| (v - median_ms).abs()).collect();
        Ok(Self {
            median_ms,
            mad_ms: median(&deviations),
            min_ms: samples_ms.iter().copied().fold(f64::INFINITY, f64::min),
            max_ms: samples_ms.iter().copied().fold(0.0, f64::max),
            samples_ms,
        })
    }

    fn validate(&self, count: usize) -> Result<()> {
        let computed = Self::new(self.samples_ms.clone())?;
        if self.samples_ms.len() != count {
            return Err("sample count differs from fixture definition".into());
        }
        for (saved, actual) in [
            (self.median_ms, computed.median_ms),
            (self.mad_ms, computed.mad_ms),
            (self.min_ms, computed.min_ms),
            (self.max_ms, computed.max_ms),
        ] {
            if !saved.is_finite() || saved < 0.0 || (saved - actual).abs() > actual.abs() * 1e-12 {
                return Err("invalid metric summary (must agree with raw samples)".into());
            }
        }
        Ok(())
    }
}

impl Budget {
    fn validate(&self) -> Result<()> {
        if [self.relative_percent, self.absolute_ms]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("budget thresholds must be finite and nonnegative".into());
        }
        Ok(())
    }

    fn regressed(&self, before: f64, after: f64) -> Result<bool> {
        self.validate()?;
        if !before.is_finite() || before < 0.0 || !after.is_finite() || after < 0.0 {
            return Err("comparison requires finite nonnegative medians".into());
        }
        let delta = after - before;
        Ok(delta > self.absolute_ms
            && (before == 0.0 || delta / before * 100.0 > self.relative_percent))
    }
}

fn validate_fixtures(f: &Fixtures) -> Result<()> {
    if f.version != 2
        || f.samples < 3
        || f.warmups < 1
        || f.units_per_file == 0
        || f.parallel_threads < 2
        || f.file_counts.is_empty()
        || f.file_counts.contains(&0)
        || f.file_counts.windows(2).any(|w| w[0] >= w[1])
    {
        return Err("invalid/unsupported performance fixture definition".into());
    }
    Ok(())
}

fn expected_metrics(f: &Fixtures) -> Vec<String> {
    let mut names = vec!["startup/version".into()];
    for scenario in SCENARIOS {
        for mode in ["fast", "strict"] {
            names.push(format!("compilation/{scenario}/{mode}"));
        }
        for files in &f.file_counts {
            for threads in [1, f.parallel_threads] {
                names.push(format!(
                    "evaluation/{scenario}/{files}-files/{threads}-threads"
                ));
                if LEGACY_SCENARIOS.contains(&scenario) {
                    for category in [
                        "subprocess-wall",
                        "subprocess-evaluation",
                        "subprocess-parse",
                    ] {
                        names.push(format!(
                            "{category}/{scenario}/{files}-files/{threads}-threads"
                        ));
                    }
                }
            }
        }
    }
    names.sort();
    names
}

fn validate_measurement(m: &Measurement) -> Result<()> {
    validate_fixtures(&m.fixtures)?;
    if m.schema_version != 1
        || m.identity.build != "release"
        || !valid_label(&m.identity.runner)
        || !valid_label(&m.identity.profile)
        || m.identity.parallel_threads != m.fixtures.parallel_threads
        || m.identity.available_threads == 0
        || [
            &m.identity.os,
            &m.identity.arch,
            &m.identity.toolchain,
            &m.identity.corpus_hash,
            &m.identity.fino_version,
            &m.revision,
            &m.binary_hash,
            &m.harness_binary_hash,
            &m.harness_source_hash,
            &m.linked_fino_version,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
    {
        return Err("invalid snapshot metadata or unsupported schema/build".into());
    }
    if m.metrics.keys().cloned().collect::<Vec<_>>() != expected_metrics(&m.fixtures) {
        return Err("missing or unexpected metric; capture a new baseline explicitly".into());
    }
    for metric in m.metrics.values() {
        metric.validate(m.fixtures.samples)?;
    }
    Ok(())
}

fn compare(before: &Measurement, after: &Measurement, budgets: &Budgets) -> Result<()> {
    validate_measurement(before)?;
    validate_measurement(after)?;
    if budgets.version != 1 {
        return Err("unsupported budget version".into());
    }
    for budget in [&budgets.startup, &budgets.evaluation, &budgets.compilation] {
        budget.validate()?;
    }
    if before.identity != after.identity
        || before.linked_fino_version != after.linked_fino_version
        || before.harness_source_hash != after.harness_source_hash
        || serde_json::to_vec(&before.fixtures)? != serde_json::to_vec(&after.fixtures)?
    {
        return Err(format!(
            "incomparable performance snapshots: runner/profile, OS/arch, thread count, toolchain, version or corpus/harness differs. Use a matching host-specific baseline; never adopt automatically.\nbaseline: {:?}\ncurrent: {:?}",
            before.identity, after.identity
        ).into());
    }
    let mut failures = Vec::new();
    for (name, current) in &after.metrics {
        let budget = if name.starts_with("startup/") || name.starts_with("subprocess-wall/") {
            &budgets.startup
        } else if name.starts_with("compilation/") {
            &budgets.compilation
        } else {
            &budgets.evaluation
        };
        let old = before.metrics[name].median_ms;
        let failed = budget.regressed(old, current.median_ms)?;
        eprintln!(
            "{} {name}: {:.3} -> {:.3} ms (MAD {:.3} ms)",
            if failed { "FAIL" } else { "ok" },
            old,
            current.median_ms,
            current.mad_ms
        );
        if failed {
            failures.push(name.clone());
        }
    }
    if !failures.is_empty() {
        return Err(format!("performance regressions: {}", failures.join(", ")).into());
    }
    Ok(())
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        // Only the harness's exclusive temporary directory; never a scanned app.
        if !Command::new("trash")
            .arg(&self.0)
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!(
                "Could not trash temporary synthetic corpus: {}",
                self.0.display()
            );
        }
    }
}

fn rules(scenario: &str) -> Vec<(String, String)> {
    let count = if scenario == "grouped" { 8 } else { 1 };
    (0..count)
        .map(|i| {
            let id = format!("perf-{i}");
            let matcher = match scenario {
                "regex-mask" => {
                    "regex = '\\bperf_marker\\b'\nmask = { nodes = ['string', 'comment'] }\n"
                }
                "root-stage" => concat!(
                    "query = '(identifier) @_expression'\n",
                    "[[stages]]\nfrom = '_expression'\nrelation = 'root'\n",
                    "unwrap_singleton = ['block']\nignore_children = ['comment']\n",
                    "query = '((identifier) @finding (#eq? @finding \"perf_marker\"))'\n",
                    "export = ['finding']\n",
                ),
                "scoped-condition" => concat!(
                    "query = '((identifier) @finding (#eq? @finding \"perf_marker\"))'\n",
                    "[[conditions]]\ncapture = 'finding'\nrelation = 'root'\n",
                    "query = '(identifier) @local'\nexpect = 'exists'\n",
                ),
                "grapheme-length" => concat!(
                    "regex = '\\bperf_marker\\b'\nmask = { nodes = ['string', 'comment'] }\n",
                    "match_length = { min = 11, max = 11, unit = 'graphemes' }\n",
                ),
                _ => "query = '((identifier) @finding (#eq? @finding \"perf_marker\"))'\n",
            };
            (
                id.clone(),
                format!("id = '{id}'\nmessage = 'Synthetic marker'\n{matcher}"),
            )
        })
        .collect()
}

fn source(index: usize, units: usize) -> String {
    format!("defmodule Synthetic{index} do\n{}end\n", UNIT.repeat(units))
}

fn expected_findings(files: &[String], units: usize, rule_count: usize) -> Vec<fino::Finding> {
    let mut findings = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let text = source(index, units);
        for (start, _) in text.match_indices("    perf_marker(") {
            let start_byte = start + 4;
            for rule in 0..rule_count {
                findings.push(fino::Finding {
                    file: file.clone(),
                    line: text[..start_byte].lines().count(),
                    column: 5,
                    rule: format!("perf-{rule}"),
                    message: "Synthetic marker".into(),
                    severity: fino::Severity::Error,
                    start_byte,
                    end_byte: start_byte + "perf_marker".len(),
                });
            }
        }
    }
    findings.sort();
    findings
}

fn scan_metric(
    engine: &Engine,
    root: &Path,
    files: &[String],
    threads: usize,
    expected: &[fino::Finding],
    f: &Fixtures,
) -> Result<Metric> {
    let snapshot = Snapshot::default();
    let mut samples = Vec::new();
    for iteration in 0..f.warmups + f.samples {
        let start = Instant::now();
        let report = engine.scan(root, Some(files), threads, &snapshot)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if report.files != files.len() || !report.errors.is_empty() || report.findings != expected {
            return Err(
                "incorrect synthetic Engine::scan output (paths, ids, spans, severity or count)"
                    .into(),
            );
        }
        if iteration >= f.warmups {
            samples.push(elapsed);
        }
    }
    Metric::new(samples)
}

fn load(config: &Path, strict: bool) -> Result<Engine> {
    let (engine, problems) = if strict {
        Engine::load(config, None)?
    } else {
        Engine::load_fast(config)?
    };
    if problems.iter().any(|p| !p.errors.is_empty()) {
        return Err(format!("synthetic rule compilation failed: {problems:?}").into());
    }
    Ok(engine)
}

fn measure(mut operation: impl FnMut() -> Result<()>, f: &Fixtures) -> Result<Metric> {
    for _ in 0..f.warmups {
        operation()?;
    }
    let mut samples = Vec::new();
    for _ in 0..f.samples {
        let start = Instant::now();
        operation()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Metric::new(samples)
}

fn command_text(program: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(format!("{} {:?} failed", program.display(), args).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

// CLI durations are cumulative elapsed spans across workers, rounded to 0.1 ms.
fn parse_timings(stderr: &str, rule_count: usize) -> Result<(f64, f64)> {
    let mut rows = BTreeMap::new();
    for line in stderr.lines().skip(1) {
        let (number, name) = line
            .trim()
            .split_once(" ms  ")
            .ok_or("malformed --timings row")?;
        let value: f64 = number.trim().parse()?;
        if !value.is_finite() || value < 0.0 || rows.insert(name, value).is_some() {
            return Err("invalid or duplicate --timings row".into());
        }
    }
    if stderr.lines().next() != Some("cumulative time across threads:")
        || rows.len() != rule_count + 2
        || !rows.contains_key("(parse)")
        || !rows.contains_key("(query pass)")
        || (0..rule_count).any(|i| !rows.contains_key(format!("perf-{i}").as_str()))
    {
        return Err("missing/unexpected --timings metrics".into());
    }
    let evaluation = rows
        .iter()
        .filter(|(name, _)| **name != "(parse)")
        .map(|(_, value)| value)
        .sum();
    Ok((evaluation, rows["(parse)"]))
}

fn subprocess_metrics(
    binary: &Path,
    root: &Path,
    files: &[String],
    threads: usize,
    scenario: &str,
    expected: &[fino::Finding],
    f: &Fixtures,
) -> Result<[Metric; 3]> {
    let mut wall = Vec::new();
    let mut evaluation = Vec::new();
    let mut parse = Vec::new();
    let rule_count = if scenario == "grouped" { 8 } else { 1 };
    for iteration in 0..f.warmups + f.samples {
        let start = Instant::now();
        let output = Command::new(binary)
            .env("RAYON_NUM_THREADS", f.parallel_threads.to_string())
            .arg("check")
            .arg("--root")
            .arg(root)
            .args(["--threads", &threads.to_string(), "--json", "--timings"])
            .args(files)
            .output()?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if output.status.code() != Some(1) {
            return Err("synthetic CLI check must exit 1 for known findings".into());
        }
        let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        if report["files"].as_u64() != Some(files.len() as u64)
            || report["errors"].as_array().is_none_or(|v| !v.is_empty())
            || report["findings"] != serde_json::to_value(expected)?
        {
            return Err("incorrect synthetic CLI output".into());
        }
        let (eval_ms, parse_ms) = parse_timings(&String::from_utf8(output.stderr)?, rule_count)?;
        if iteration >= f.warmups {
            wall.push(elapsed);
            evaluation.push(eval_ms);
            parse.push(parse_ms);
        }
    }
    Ok([
        Metric::new(wall)?,
        Metric::new(evaluation)?,
        Metric::new(parse)?,
    ])
}

fn collect(runner: String, profile: String, binary: &Path, f: Fixtures) -> Result<Measurement> {
    if cfg!(debug_assertions) {
        return Err("performance harness must be built with --release".into());
    }
    let binary = fs::canonicalize(binary)?;
    if binary.components().any(|c| c.as_os_str() == "debug") {
        return Err(
            "refusing obvious debug binary; --binary must name an optimized release artifact"
                .into(),
        );
    }
    let binary_bytes = fs::read(&binary)?;
    let harness_path = std::env::current_exe()?;
    let harness_bytes = fs::read(&harness_path)?;
    let fino_version = command_text(&binary, &["--version"])?;
    if !fino_version.starts_with("fino ") {
        return Err("binary --version did not identify Fino".into());
    }
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "fino-performance-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos()
    )));
    fs::create_dir(&scratch.0)?;
    let mut metrics = BTreeMap::new();
    metrics.insert(
        "startup/version".into(),
        measure(
            || {
                if command_text(&binary, &["--version"])? != fino_version {
                    return Err("binary version changed during run".into());
                }
                Ok(())
            },
            &f,
        )?,
    );
    let mut corpus = serde_json::to_vec(&f)?;
    corpus.extend_from_slice(UNIT.as_bytes());
    for scenario in SCENARIOS {
        let root = scratch.0.join(scenario);
        let rule_dir = root.join(".fino/rules");
        fs::create_dir_all(&rule_dir)?;
        let config = root.join(".fino/config.toml");
        let config_text = "include = ['*.ex']\nrules_dir = 'rules'\n";
        fs::write(&config, config_text)?;
        corpus.extend_from_slice(config_text.as_bytes());
        for (id, text) in rules(scenario) {
            corpus.extend_from_slice(id.as_bytes());
            corpus.extend_from_slice(text.as_bytes());
            fs::write(rule_dir.join(format!("{id}.toml")), text)?;
        }
        for strict in [false, true] {
            metrics.insert(
                format!(
                    "compilation/{scenario}/{}",
                    if strict { "strict" } else { "fast" }
                ),
                measure(
                    || {
                        std::hint::black_box(load(&config, strict)?);
                        Ok(())
                    },
                    &f,
                )?,
            );
        }
        let engine = load(&config, false)?;
        let mut paths = Vec::new();
        for index in 0..*f.file_counts.last().ok_or("no corpus sizes")? {
            let name = format!("fixture-{index:04}.ex");
            let source = source(index, f.units_per_file);
            corpus.extend_from_slice(name.as_bytes());
            corpus.extend_from_slice(source.as_bytes());
            fs::write(root.join(&name), source)?;
            paths.push(name);
        }
        for &files in &f.file_counts {
            for threads in [1, f.parallel_threads] {
                let expected = expected_findings(
                    &paths[..files],
                    f.units_per_file,
                    if scenario == "grouped" { 8 } else { 1 },
                );
                let metric = scan_metric(&engine, &root, &paths[..files], threads, &expected, &f)?;
                metrics.insert(
                    format!("evaluation/{scenario}/{files}-files/{threads}-threads"),
                    metric,
                );
                if !LEGACY_SCENARIOS.contains(&scenario) {
                    continue;
                }
                let [wall, evaluation, parse] = subprocess_metrics(
                    &binary,
                    &root,
                    &paths[..files],
                    threads,
                    scenario,
                    &expected,
                    &f,
                )?;
                for (category, metric) in [
                    ("subprocess-wall", wall),
                    ("subprocess-evaluation", evaluation),
                    ("subprocess-parse", parse),
                ] {
                    metrics.insert(
                        format!("{category}/{scenario}/{files}-files/{threads}-threads"),
                        metric,
                    );
                }
            }
        }
    }
    if fs::read(&binary)? != binary_bytes || fs::read(&harness_path)? != harness_bytes {
        return Err("performance artifacts changed during collection; discard this run".into());
    }
    let result = Measurement {
        schema_version: 1,
        identity: Identity {
            runner,
            profile,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            available_threads: std::thread::available_parallelism()?.get(),
            parallel_threads: f.parallel_threads,
            build: "release".into(),
            toolchain: command_text(Path::new("rustc"), &["--version"])?,
            corpus_hash: hash(&corpus),
            fino_version,
        },
        revision: command_text(Path::new("git"), &["rev-parse", "HEAD"])?,
        binary_hash: hash(&binary_bytes),
        harness_binary_hash: hash(&harness_bytes),
        harness_source_hash: hash(include_bytes!("performance.rs")),
        linked_fino_version: env!("CARGO_PKG_VERSION").into(),
        captured_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        fixtures: f,
        metrics,
    };
    validate_measurement(&result)?;
    Ok(result)
}

fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let mut binary = PathBuf::from("target/release/fino");
    if let Some(index) = args.iter().position(|a| a == "--binary") {
        if index + 1 >= args.len() {
            return Err("--binary needs an optimized release artifact path".into());
        }
        binary = PathBuf::from(args.remove(index + 1));
        args.remove(index);
    }
    if args.len() < 3 || !["run", "snapshot", "check"].contains(&args[0].as_str()) {
        return Err("usage: performance <run|snapshot|check> RUNNER PROFILE [BASELINE.json] [--binary RELEASE_ARTIFACT]; run prints JSON; snapshot explicitly creates/replaces BASELINE; check never writes it. Run from the repository root. --binary asserts a release build; obvious debug paths are refused.".into());
    }
    let mode = &args[0];
    if !valid_label(&args[1])
        || !valid_label(&args[2])
        || args.len() != if mode == "run" { 3 } else { 4 }
    {
        return Err(
            "supply generic RUNNER PROFILE labels (lowercase letters, digits, hyphens) and a baseline path only for snapshot/check"
                .into(),
        );
    }
    let f: Fixtures = read_json(Path::new("performance/fixtures.json"))?;
    validate_fixtures(&f)?;
    // Missing/malformed baselines fail before doing an expensive run.
    let baseline: Option<Measurement> = if mode == "check" {
        let m = read_json(Path::new(&args[3]))?;
        validate_measurement(&m)?;
        Some(m)
    } else {
        None
    };
    let budgets: Budgets = read_json(Path::new("performance/budgets.json"))?;
    if budgets.version != 1 {
        return Err("unsupported budgets version".into());
    }
    for b in [&budgets.startup, &budgets.evaluation, &budgets.compilation] {
        b.validate()?;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(f.parallel_threads)
        .build_global()?;
    let current = collect(args[1].clone(), args[2].clone(), &binary, f)?;
    if let Some(baseline) = baseline {
        // Preserve this run's raw samples even when the comparison fails.
        println!("{}", serde_json::to_string_pretty(&current)?);
        compare(&baseline, &current, &budgets)?;
    } else {
        let text = serde_json::to_string_pretty(&current)? + "\n";
        if mode == "snapshot" {
            fs::write(&args[3], text)?;
            println!("Explicitly saved performance baseline: {}", args[3]);
        } else {
            print!("{text}");
        }
    }
    Ok(())
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_timing_input_is_complete_and_nonnegative() {
        let text = "cumulative time across threads:\n  1.2 ms  perf-0\n  2.0 ms  (parse)\n  0.0 ms  (query pass)\n";
        assert_eq!(parse_timings(text, 1).unwrap(), (1.2, 2.0));
        assert!(parse_timings(text, 2).is_err());
        for bad in ["NaN", "inf", "-1.0"] {
            assert!(parse_timings(&text.replace("1.2", bad), 1).is_err());
        }
        assert!(parse_timings(&text.replace("(parse)", "other"), 1).is_err());
        assert!(parse_timings(&(text.to_string() + "  1.0 ms  perf-0\n"), 1).is_err());
    }

    #[test]
    fn regression_requires_both_thresholds_strictly_exceeded() {
        let b = Budget {
            relative_percent: 20.0,
            absolute_ms: 2.0,
        };
        assert!(!b.regressed(1.0, 2.0).unwrap());
        assert!(!b.regressed(100.0, 110.0).unwrap());
        assert!(!b.regressed(10.0, 12.0).unwrap());
        assert!(b.regressed(10.0, 12.1).unwrap());
        assert!(!b.regressed(10.0, 9.0).unwrap());
        assert!(!b.regressed(0.0, 1.0).unwrap());
        assert!(b.regressed(0.0, 3.0).unwrap());
        assert!(b.regressed(f64::NAN, 3.0).is_err());
        assert!(b.regressed(1.0, f64::INFINITY).is_err());
    }

    #[test]
    fn invalid_numbers_fail_closed() {
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(
                Budget {
                    relative_percent: bad,
                    absolute_ms: 0.0
                }
                .validate()
                .is_err()
            );
            assert!(
                Budget {
                    relative_percent: 0.0,
                    absolute_ms: bad
                }
                .validate()
                .is_err()
            );
            assert!(Metric::new(vec![1.0, bad, 2.0]).is_err());
        }
        assert!(Metric::new(vec![0.0, 1.0, 2.0]).is_ok());
        assert!(Metric::new(vec![1.0]).is_err());
        assert!(
            serde_json::from_str::<Budget>(r#"{"relative_percent":null,"absolute_ms":1}"#).is_err()
        );
        assert!(serde_json::from_str::<Budget>(r#"{"absolute_ms":1}"#).is_err());
    }

    #[test]
    fn summaries_are_recomputed_not_trusted() {
        let mut m = Metric::new(vec![1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
        assert_eq!(m.median_ms, 3.0);
        assert_eq!(m.mad_ms, 1.0);
        assert!(m.validate(5).is_ok());
        assert!(m.validate(6).is_err());
        m.median_ms = 1.0;
        assert!(m.validate(5).is_err());
    }

    fn sample() -> Measurement {
        let fixtures = Fixtures {
            version: 2,
            file_counts: vec![1],
            units_per_file: 1,
            samples: 3,
            warmups: 1,
            parallel_threads: 4,
        };
        Measurement {
            schema_version: 1,
            identity: Identity {
                runner: "dedicated-a".into(),
                profile: "default".into(),
                os: "linux".into(),
                arch: "aarch64".into(),
                available_threads: 8,
                parallel_threads: 4,
                build: "release".into(),
                toolchain: "rustc test".into(),
                corpus_hash: "hash-a".into(),
                fino_version: "fino 0.1.0".into(),
            },
            revision: "rev".into(),
            binary_hash: "hash-c".into(),
            harness_binary_hash: "hash-d".into(),
            harness_source_hash: "hash-e".into(),
            linked_fino_version: "0.1.0".into(),
            captured_unix_seconds: 1,
            metrics: expected_metrics(&fixtures)
                .into_iter()
                .map(|n| (n, Metric::new(vec![1.0, 2.0, 3.0]).unwrap()))
                .collect(),
            fixtures,
        }
    }

    #[test]
    fn comparison_rejects_missing_metrics_and_incomparable_hosts_or_corpora() {
        let before = sample();
        let budget = || Budget {
            relative_percent: 20.0,
            absolute_ms: 1.0,
        };
        let budgets = Budgets {
            version: 1,
            startup: budget(),
            evaluation: budget(),
            compilation: budget(),
        };
        assert!(compare(&before, &before, &budgets).is_ok());
        for field in ["runner", "profile", "os", "build", "corpus", "collector"] {
            let mut after = before.clone();
            match field {
                "runner" => after.identity.runner = "dedicated-b".into(),
                "profile" => after.identity.profile = "other".into(),
                "os" => after.identity.os = "macos".into(),
                "build" => after.identity.build = "debug".into(),
                "collector" => after.harness_source_hash = "changed-method".into(),
                _ => after.identity.corpus_hash = "other".into(),
            }
            assert!(compare(&before, &after, &budgets).is_err());
        }
        for name in [
            "startup/version",
            "evaluation/root-stage/1-files/1-threads",
            "compilation/grapheme-length/fast",
        ] {
            let mut missing = before.clone();
            missing.metrics.remove(name);
            assert!(compare(&missing, &before, &budgets).is_err());
            assert!(compare(&before, &missing, &budgets).is_err());
        }
        let mut wrong_producer = before.clone();
        wrong_producer.metrics.insert(
            "subprocess-wall/scoped-condition/1-files/1-threads".into(),
            Metric::new(vec![1.0, 2.0, 3.0]).unwrap(),
        );
        assert!(compare(&before, &wrong_producer, &budgets).is_err());
        let mut rebuilt = before.clone();
        rebuilt.binary_hash = "different-binary".into();
        rebuilt.harness_binary_hash = "different-linked-library".into();
        assert!(compare(&before, &rebuilt, &budgets).is_ok());
    }
}
