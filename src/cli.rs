//! One command definition drives parsing, help, and the Usage completion spec.
use clap::{Arg, ArgAction, Command, ValueHint};
use fino::Result;
use std::path::PathBuf;

pub struct Args {
    pub command: String,
    pub root: PathBuf,
    pub config: Option<PathBuf>,
    pub threads: usize,
    pub manifest: Option<PathBuf>,
    pub code: Option<String>,
    pub json: bool,
    pub color: String,
    pub context: usize,
    pub group_by: String,
    pub timings: bool,
    pub update: bool,
    pub runs: usize,
    pub positional: Vec<String>,
}

fn value(name: &'static str, label: &'static str, help: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name(label)
        .help(help)
        .allow_hyphen_values(true)
}

fn command(name: &'static str, help: &'static str, files: &'static str) -> Command {
    // Keep the legacy common options accepted on every command. The command
    // handlers decide which options apply, as they did before the parser change.
    Command::new(name)
        .about(help)
        .args_override_self(true)
        .arg(value("root", "PATH", "Project root (default: .)").value_hint(ValueHint::DirPath))
        .arg(
            value(
                "config",
                "FILE",
                "Config file (default: ROOT/.fino/config.toml)",
            )
            .value_hint(ValueHint::FilePath),
        )
        .arg(
            value("threads", "N", "Worker threads (default: available CPUs)")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            value(
                "files-from",
                "FILE",
                "Relative paths, one per line (check, snapshot, or profile)",
            )
            .value_hint(ValueHint::FilePath),
        )
        .arg(value(
            "code",
            "SOURCE",
            "Parse source instead of a file (tree)",
        ))
        .arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .help("Machine-readable output"),
        )
        .arg(
            Arg::new("timings")
                .long("timings")
                .action(ArgAction::SetTrue)
                .help("Print cumulative timings with shared query traversal (check or snapshot); use profile for independent rule costs"),
        )
        .arg(
            value("color", "WHEN", "Human output colors (ignored by --json)")
                .value_parser(["auto", "always", "never"])
                .default_value("auto"),
        )
        .arg(
            value(
                "context",
                "N",
                "Source context lines; 0 hides snippets (check or snapshot)",
            )
            .value_parser(clap::value_parser!(usize))
            .default_value("2"),
        )
        .arg(
            value(
                "group-by",
                "ORDER",
                "Group findings with rule descriptions (human output only)",
            )
            .value_parser(["rule", "none"])
            .default_value("none"),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .action(ArgAction::SetTrue)
                .help("Remove stale entries; never adopt new violations (snapshot)"),
        )
        .arg(
            Arg::new("files")
                .value_name(files)
                .num_args(0..)
                .action(ArgAction::Append)
                .value_hint(ValueHint::FilePath),
        )
}

pub fn definition() -> Command {
    Command::new("fino")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Declarative, parallel Elixir linting")
        .after_help("Exit codes: 0 ok, 1 new violations or failing tests, 2 invalid config/rules, read or parse errors.")
        .arg(Arg::new("usage-spec").long("usage-spec").action(ArgAction::SetTrue).help("Print the Usage spec for shell completion and documentation"))
        .subcommand(command("check", "Lint the project; never writes", "FILES"))
        .subcommand(command("profile", "Rank independent rule costs; one warmup, no snapshot writes; findings do not fail", "FILES")
            .mut_arg("threads", |arg| arg.help("Worker threads (profile default: 1)").default_value("1"))
            .arg(value("runs", "N", "Measured scans after one warmup (positive integer)")
                .value_parser(clap::value_parser!(u32).range(1..)).default_value("3")))
        .subcommand(command("snapshot", "Explicitly adopt current violations, or prune with --update", "FILES"))
        .subcommand(command("validate", "Validate config and all (or selected) rule files", "RULE_FILES"))
        .subcommand(command("test", "Run examples in all (or selected) rule files", "RULE_FILES"))
        .subcommand(command("tree", "Print a syntax tree from FILE, - (stdin), or --code SOURCE", "FILE"))
        .subcommand(command("schema", "Print the JSON Schema for rule or config files", "KIND")
             .mut_arg("files", |arg| arg.value_parser(["rule", "config"]).value_hint(ValueHint::Unknown)))
        .subcommand(command("skills", "Discover agent skills or print a portable SKILL.md (rules or config)", "TOPIC")
            .mut_arg("files", |arg| arg.num_args(0..=1).value_parser(["rules", "config"]).value_hint(ValueHint::Unknown)))
}

pub fn parse() -> Result<Option<Args>> {
    let mut cli = definition();
    let matches = match cli.try_get_matches_from_mut(std::env::args_os()) {
        Ok(matches) => matches,
        Err(error) if !error.use_stderr() => {
            print!("{error}");
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    if matches.get_flag("usage-spec") {
        clap_usage::generate(&mut cli, "fino", &mut std::io::stdout());
        return Ok(None);
    }
    let Some((command, matches)) = matches.subcommand() else {
        cli.print_help().map_err(|e| e.to_string())?;
        println!();
        return Ok(None);
    };
    Ok(Some(Args {
        command: command.to_owned(),
        root: matches
            .get_one::<String>("root")
            .map_or_else(|| PathBuf::from("."), PathBuf::from),
        config: matches.get_one::<String>("config").map(PathBuf::from),
        threads: matches
            .get_one::<usize>("threads")
            .copied()
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from)),
        manifest: matches.get_one::<String>("files-from").map(PathBuf::from),
        code: matches.get_one::<String>("code").cloned(),
        json: matches.get_flag("json"),
        color: matches.get_one::<String>("color").unwrap().clone(),
        context: *matches.get_one::<usize>("context").unwrap(),
        group_by: matches.get_one::<String>("group-by").unwrap().clone(),
        timings: matches.get_flag("timings"),
        update: matches.get_flag("update"),
        runs: if command == "profile" {
            *matches.get_one::<u32>("runs").unwrap() as usize
        } else {
            3
        },
        positional: matches
            .get_many::<String>("files")
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
    }))
}
