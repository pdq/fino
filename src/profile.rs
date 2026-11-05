//! Independent rule timing, using one normal multi-rule scan per sample.
use crate::{cli::Args, output::safe};
use fino::{Engine, Result, Snapshot};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const MODE: &str = "independent rule-query evaluation; query sharing disabled";
const ATTRIBUTION: &str = "Cumulative elapsed time across workers, not CPU time or additive wall-time attribution. Not marginal savings from removing a rule.";

#[derive(Serialize)]
struct RuleTiming {
    rule: String,
    median_ms: f64,
    samples_ms: Vec<f64>,
    findings: usize,
    timing_status: &'static str,
}

#[derive(Serialize)]
struct Profile {
    schema_version: u32,
    mode: &'static str,
    attribution: &'static str,
    requested_runs: usize,
    completed_runs: usize,
    warmup_completed: bool,
    threads: usize,
    files: usize,
    rule_count: usize,
    findings: usize,
    scan_wall_samples_ms: Vec<f64>,
    scan_wall_median_ms: Option<f64>,
    parse_samples_ms: Vec<f64>,
    parse_median_ms: Option<f64>,
    rules: Vec<RuleTiming>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    })
}

fn timings(engine: &Engine) -> BTreeMap<String, Duration> {
    engine.timings().into_iter().collect()
}

fn diagnostic(error: fino::Diagnostic) -> String {
    format!(
        "{}:{}:{}: {}",
        error.file, error.line, error.column, error.message
    )
}

fn measure(profile: &mut Profile, args: &Args, config: &Path) -> Result<()> {
    if args.update {
        return Err("--update is only supported by snapshot".into());
    }
    if args.threads == 0 {
        return Err("profile --threads must be positive".into());
    }
    // Strict loading compiles each query and leaves query groups disabled.
    let (engine, problems) = Engine::load(config, None)?;
    for problem in problems {
        profile.errors.extend(
            problem
                .errors
                .into_iter()
                .map(|error| format!("{}: {error}", problem.file)),
        );
        profile.warnings.extend(
            problem
                .warnings
                .into_iter()
                .map(|warning| format!("{}: {warning}", problem.file)),
        );
    }
    profile.rule_count = engine.rules.len();
    if !profile.errors.is_empty() {
        return Ok(());
    }
    if engine.rules.is_empty() {
        return Err("no rules found".into());
    }
    let files = crate::selected_files(args)?;
    let snapshot = Snapshot::default();
    let warmup = engine.scan(&args.root, files.as_deref(), args.threads, &snapshot)?;
    if !warmup.errors.is_empty() {
        profile
            .errors
            .extend(warmup.errors.into_iter().map(diagnostic));
        return Ok(());
    }
    profile.warmup_completed = true;
    drop(warmup);
    profile.rules = engine
        .rules
        .iter()
        .map(|rule| RuleTiming {
            rule: rule.id.clone(),
            median_ms: 0.0,
            samples_ms: Vec::new(),
            findings: 0,
            timing_status: "no_recorded_time",
        })
        .collect();
    for _ in 0..args.runs {
        let before = timings(&engine);
        let started = Instant::now();
        let report = engine.scan(&args.root, files.as_deref(), args.threads, &snapshot)?;
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        if !report.errors.is_empty() {
            profile
                .errors
                .extend(report.errors.into_iter().map(diagnostic));
            return Ok(());
        }
        let after = timings(&engine);
        let delta = |name: &str| {
            after
                .get(name)
                .copied()
                .unwrap_or_default()
                .saturating_sub(before.get(name).copied().unwrap_or_default())
                .as_secs_f64()
                * 1000.0
        };
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for finding in &report.findings {
            *counts.entry(&finding.rule).or_default() += 1;
        }
        for row in &mut profile.rules {
            let elapsed = delta(&row.rule);
            row.samples_ms.push(elapsed);
            row.findings = counts.get(row.rule.as_str()).copied().unwrap_or(0);
            if elapsed > 0.0 {
                row.timing_status = "recorded";
            }
        }
        profile.files = report.files;
        profile.findings = report.findings.len();
        profile.completed_runs += 1;
        profile.scan_wall_samples_ms.push(wall_ms);
        profile.parse_samples_ms.push(delta("(parse)"));
    }
    Ok(())
}

pub fn run(args: &Args, config: &Path) -> Result<i32> {
    let mut profile = Profile {
        schema_version: 1,
        mode: MODE,
        attribution: ATTRIBUTION,
        requested_runs: args.runs,
        completed_runs: 0,
        warmup_completed: false,
        threads: args.threads,
        files: 0,
        rule_count: 0,
        findings: 0,
        scan_wall_samples_ms: Vec::new(),
        scan_wall_median_ms: None,
        parse_samples_ms: Vec::new(),
        parse_median_ms: None,
        rules: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };
    if let Err(error) = measure(&mut profile, args, config) {
        profile.errors.push(error);
    }
    profile.scan_wall_median_ms = median(&profile.scan_wall_samples_ms);
    profile.parse_median_ms = median(&profile.parse_samples_ms);
    for row in &mut profile.rules {
        row.median_ms = median(&row.samples_ms).unwrap_or(0.0);
    }
    profile.rules.sort_by(|a, b| {
        b.median_ms
            .total_cmp(&a.median_ms)
            .then(a.rule.cmp(&b.rule))
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&profile).map_err(|e| e.to_string())?
        );
    } else {
        println!("Profile: {MODE}\n{ATTRIBUTION}");
        println!(
            "{} files, {} rules, {} findings (last completed measured scan); {} measured runs, {} threads",
            profile.files,
            profile.rule_count,
            profile.findings,
            profile.completed_runs,
            profile.threads
        );
        if let (Some(wall), Some(parse)) = (profile.scan_wall_median_ms, profile.parse_median_ms) {
            println!("Median scan wall: {wall:.3} ms; median cumulative parsing: {parse:.3} ms");
        }
        println!("  Median ms  Findings  Rule");
        for row in &profile.rules {
            println!(
                "  {:>9.3}  {:>8}  {}{}",
                row.median_ms,
                row.findings,
                safe(&row.rule),
                if row.timing_status == "no_recorded_time" {
                    " (no recorded time; skipped or below clock resolution)"
                } else {
                    ""
                }
            );
        }
        for warning in &profile.warnings {
            eprintln!("warning: {}", safe(warning));
        }
        for error in &profile.errors {
            eprintln!("error: {}", safe(error));
        }
    }
    Ok(if profile.errors.is_empty() { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::median;

    #[test]
    fn medians_cover_odd_even_empty_and_unsorted_samples() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }
}
