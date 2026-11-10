//! Embed the same portable skills that the Skills CLI discovers in the repository.
use crate::cli::Args;
use fino::Result;
use serde::Serialize;

#[derive(Serialize)]
struct Skill {
    name: &'static str,
    topic: &'static str,
    content: &'static str,
}

const SKILLS: &[Skill] = &[
    Skill {
        name: "fino-rules",
        topic: "rules",
        content: include_str!("../skills/fino-rules/SKILL.md"),
    },
    Skill {
        name: "fino-config",
        topic: "config",
        content: include_str!("../skills/fino-config/SKILL.md"),
    },
];

pub fn run(args: &Args) -> Result<i32> {
    let selected = args.positional.first().map(String::as_str);
    let skills: Vec<_> = SKILLS
        .iter()
        .filter(|skill| selected.is_none_or(|topic| skill.topic == topic))
        .collect();
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&skills).map_err(|e| e.to_string())?
        );
    } else if selected.is_some() {
        print!("{}", skills[0].content);
    } else {
        println!("Agent skills (print with `fino skills TOPIC`):");
        println!("  rules   fino-rules   Create, test, and optimize lint rules");
        println!("  config  fino-config  Configure projects, suppressions, and snapshots");
        println!("\nInstall: aubx skills add pdqcom/fino --skill fino-rules fino-config");
    }
    Ok(0)
}
