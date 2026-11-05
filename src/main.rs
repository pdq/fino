use fino::model::{ProjectConfig, RuleFile};
use fino::{Engine, Problems, Report, Result, Snapshot};
use std::path::{Path, PathBuf};
use std::{env, fs, io::Read, process};

const HELP: &str = "fino — declarative, parallel Elixir linting

Usage: fino <command> [options] [files...]

Commands:
  check              Lint the project; never writes
  snapshot           Explicitly adopt current violations (optionally given files)
  snapshot --update  Remove stale entries only; never adopt new violations
  validate [FILES]   Validate the config and all (or the given) rule files
  test [FILES]       Run the [[tests]] examples of all (or the given) rule files
  tree FILE|-        Print the syntax tree (node kinds, fields, positions)
  schema rule|config Print the JSON Schema for rule or config files

Options:
  --root PATH        Project root (default: .)
  --config FILE      Config file (default: ROOT/.fino/config.toml)
  --threads N        Worker threads (default: available CPUs)
  --files-from FILE  Relative paths, one per line (check or snapshot)
  --code SOURCE      tree only: parse SOURCE instead of a file
  --json             Machine-readable output
  --timings          check only: print cumulative time per rule to stderr

Exit codes: 0 ok, 1 new violations or failing tests, 2 invalid config/rules,
read or parse errors.
";

struct Args {
    command: String,
    root: PathBuf,
    config: Option<PathBuf>,
    threads: usize,
    manifest: Option<PathBuf>,
    code: Option<String>,
    json: bool,
    timings: bool,
    update: bool,
    positional: Vec<String>,
}

fn main() {
    match run() {
        Ok(code) => process::exit(code),
        Err(message) => {
            eprintln!("fino: {message}");
            process::exit(2);
        }
    }
}

fn parse_args() -> Result<Option<Args>> {
    let mut raw = env::args().skip(1);
    let Some(command) = raw.next() else {
        print!("{HELP}");
        return Ok(None);
    };
    match command.as_str() {
        "--version" | "-V" => {
            println!("fino {}", env!("CARGO_PKG_VERSION"));
            return Ok(None);
        }
        "--help" | "-h" | "help" => {
            print!("{HELP}");
            return Ok(None);
        }
        _ => {}
    }
    let mut args = Args {
        command,
        root: PathBuf::from("."),
        config: None,
        threads: std::thread::available_parallelism().map_or(1, usize::from),
        manifest: None,
        code: None,
        json: false,
        timings: false,
        update: false,
        positional: Vec::new(),
    };
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--json" => args.json = true,
            "--timings" => args.timings = true,
            "--update" => args.update = true,
            "--root" => args.root = PathBuf::from(value()?),
            "--config" => args.config = Some(PathBuf::from(value()?)),
            "--files-from" => args.manifest = Some(PathBuf::from(value()?)),
            "--code" => args.code = Some(value()?),
            "--threads" => args.threads = value()?.parse().map_err(|_| "invalid thread count")?,
            other if other.starts_with("--") => return Err(format!("unknown option {other:?}")),
            _ => args.positional.push(flag),
        }
    }
    Ok(Some(args))
}

fn run() -> Result<i32> {
    let Some(args) = parse_args()? else {
        return Ok(0);
    };
    let config = args
        .config
        .clone()
        .unwrap_or_else(|| args.root.join(".fino/config.toml"));
    match args.command.as_str() {
        "check" | "snapshot" => lint(&args, &config),
        "validate" => validate(&args, &config),
        "test" => test(&args, &config),
        "tree" => tree(&args),
        "schema" => schema(&args),
        other => Err(format!("unknown command {other:?}; see --help")),
    }
}

fn only_files(args: &Args) -> Option<Vec<PathBuf>> {
    (!args.positional.is_empty()).then(|| args.positional.iter().map(PathBuf::from).collect())
}

fn report_problems(problems: &[Problems], json: bool) -> bool {
    let failed = problems.iter().any(|p| !p.errors.is_empty());
    if json {
        println!("{}", serde_json::to_string(problems).unwrap_or_default());
    } else {
        for p in problems {
            for e in &p.errors {
                eprintln!("{}: error: {e}", p.file);
            }
            for w in &p.warnings {
                eprintln!("{}: warning: {w}", p.file);
            }
        }
    }
    failed
}

fn load(
    config: &Path,
    only: Option<&[PathBuf]>,
    json: bool,
) -> Result<std::result::Result<Engine, i32>> {
    let (engine, problems) = Engine::load(config, only)?;
    if problems.iter().any(|p| !p.errors.is_empty()) {
        report_problems(&problems, json);
        return Ok(Err(2));
    }
    Ok(Ok(engine))
}

fn validate(args: &Args, config: &Path) -> Result<i32> {
    let only = only_files(args);
    let (engine, problems) = Engine::load(config, only.as_deref())?;
    let failed = report_problems(&problems, args.json);
    if !args.json {
        let warnings: usize = problems.iter().map(|p| p.warnings.len()).sum();
        println!(
            "{}: {} rule files, {} valid, {} warnings",
            if failed { "INVALID" } else { "OK" },
            problems.len(),
            engine.rules.len(),
            warnings
        );
    }
    Ok(if failed { 2 } else { 0 })
}

fn test(args: &Args, config: &Path) -> Result<i32> {
    let only = only_files(args);
    let engine = match load(config, only.as_deref(), args.json)? {
        Ok(engine) => engine,
        Err(code) => return Ok(code),
    };
    let outcomes = fino::run_tests(&engine);
    let failed = outcomes.iter().filter(|o| !o.passed).count();
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&outcomes).map_err(|e| e.to_string())?
        );
    } else {
        for o in outcomes.iter().filter(|o| !o.passed) {
            match &o.error {
                Some(e) => println!("FAIL {} — {}: {e}", o.rule, o.name),
                None => println!(
                    "FAIL {} — {}: expected lines {:?}, got {:?}",
                    o.rule, o.name, o.expected, o.actual
                ),
            }
        }
        println!(
            "{}: {} tests, {} failed, {} rules",
            if failed == 0 { "PASS" } else { "FAIL" },
            outcomes.len(),
            failed,
            engine.rules.len()
        );
    }
    Ok(if failed == 0 { 0 } else { 1 })
}

fn tree(args: &Args) -> Result<i32> {
    let source = match (&args.code, args.positional.first().map(String::as_str)) {
        (Some(code), _) => code.clone(),
        (None, Some("-")) => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            text
        }
        (None, Some(file)) => {
            fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?
        }
        (None, None) => return Err("tree needs FILE, - or --code SOURCE".into()),
    };
    print!("{}", fino::render_tree(&source)?);
    Ok(0)
}

fn schema(args: &Args) -> Result<i32> {
    let generator = schemars::generate::SchemaSettings::draft07().into_generator();
    let schema = match args.positional.first().map(String::as_str) {
        Some("rule") => generator.into_root_schema_for::<RuleFile>(),
        Some("config") => generator.into_root_schema_for::<ProjectConfig>(),
        _ => return Err("schema needs `rule` or `config`".into()),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())?
    );
    Ok(0)
}

fn lint(args: &Args, config: &Path) -> Result<i32> {
    if args.update && args.command != "snapshot" {
        return Err("--update is only supported by snapshot".into());
    }
    let (engine, problems) = Engine::load_fast(config)?;
    if problems.iter().any(|p| !p.errors.is_empty()) {
        report_problems(&problems, args.json);
        return Ok(2);
    }
    if engine.rules.is_empty() {
        return Err("no rules found".into());
    }
    let snapshot = Snapshot::load(&engine.snapshot_path)?;
    let mut files = args
        .manifest
        .as_ref()
        .map(|path| {
            fs::read_to_string(path)
                .map(|text| {
                    text.lines()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .map_err(|e| format!("cannot read file manifest: {e}"))
        })
        .transpose()?;
    if !args.positional.is_empty() {
        files
            .get_or_insert_with(Vec::new)
            .extend(args.positional.iter().cloned());
    }
    let mut report = engine.scan(&args.root, files.as_deref(), args.threads, &snapshot)?;
    if report.errors.is_empty() && args.command == "snapshot" {
        snapshot.write_update(
            &engine.snapshot_path,
            &report.stale,
            &report.current,
            !args.update,
        )?;
        if !args.update {
            report.new.clear();
            report.passed = true;
        }
        report.stale.clear();
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        );
    } else {
        human(&report);
    }
    if args.timings {
        eprintln!("cumulative time across threads:");
        for (name, time) in engine.timings() {
            eprintln!("  {:>10.1} ms  {name}", time.as_secs_f64() * 1000.0);
        }
    }
    Ok(report.exit_code())
}

fn human(report: &Report) {
    for row in &report.findings {
        let tag = if row.severity == fino::Severity::Warning {
            " (warning)"
        } else {
            ""
        };
        println!(
            "{}:{}:{}: {}{tag}: {}",
            row.file, row.line, row.column, row.rule, row.message
        );
    }
    for error in &report.errors {
        eprintln!(
            "{}:{}:{}: error: {}",
            error.file, error.line, error.column, error.message
        );
    }
    for entry in &report.stale {
        eprintln!(
            "{}: {}: stale snapshot entry (run fino snapshot --update)",
            entry.file, entry.rule
        );
    }
    println!(
        "{}: {} files, {} findings, {} errors, {:.2} ms ({})",
        if report.passed { "PASS" } else { "FAIL" },
        report.files,
        report.findings.len(),
        report.errors.len(),
        report.elapsed_ms,
        report.scope
    );
}
