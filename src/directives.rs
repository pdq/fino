//! Suppression comments: native `# fino:` directives and, optionally,
//! Credo's `# credo:disable-for-*` comments (same semantics as Credo 1.7).
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

#[derive(Default)]
pub(crate) struct Directives {
    file: BTreeSet<String>,
    lines: BTreeMap<usize, BTreeSet<String>>,
    credo: Vec<CredoDirective>,
}

struct CredoDirective {
    lines: Option<(i64, i64)>,
    target: CredoTarget,
}

enum CredoTarget {
    All,
    Name(String),
    Pattern(regex::Regex),
}

static CREDO: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)#\s*credo:([A-Za-z0-9_\-:]+)\s*(.*)").expect("valid regex")
});

impl Directives {
    /// Parse one comment. Returns an error message for malformed native directives.
    pub fn add_comment(
        &mut self,
        comment: &str,
        line: usize,
        known: &BTreeSet<&str>,
        credo: bool,
    ) -> Result<(), String> {
        if credo && let Some(found) = CREDO.captures(comment) {
            self.add_credo(&found[1], &found[2], line as i64);
        }
        let Some(value) = comment
            .trim_start_matches('#')
            .trim_start()
            .strip_prefix("fino:")
        else {
            return Ok(());
        };
        let (instruction, arguments) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
        let target = match instruction {
            "disable-file" => None,
            "disable-next-line" => Some(line + 1),
            "disable-line" => Some(line),
            _ => return Err("unknown fino directive".into()),
        };
        let arguments = arguments
            .split_once(" -- ")
            .map_or(arguments, |(ids, _)| ids);
        let names: BTreeSet<String> = arguments
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if names.is_empty()
            || names
                .iter()
                .any(|id| id != "all" && !known.contains(id.as_str()))
        {
            return Err("directive needs known rule ids or all".into());
        }
        match target {
            Some(line) => self.lines.entry(line).or_default().extend(names),
            None => self.file.extend(names),
        }
        Ok(())
    }

    fn add_credo(&mut self, instruction: &str, params: &str, line: i64) {
        let lines = match instruction {
            "disable-for-this-file" => None,
            "disable-for-next-line" => Some((line + 1, line + 1)),
            "disable-for-previous-line" => Some((line - 1, line - 1)),
            _ => match instruction
                .strip_prefix("disable-for-lines:")
                .and_then(|n| n.parse::<i64>().ok())
            {
                Some(n) if n >= 0 => Some((line, line + n)),
                Some(n) => Some((line + n, line)),
                None => return,
            },
        };
        let target = if params.is_empty() {
            CredoTarget::All
        } else if params.len() > 2 && params.starts_with('/') && params.ends_with('/') {
            match regex::Regex::new(&format!("(?i){}", &params[1..params.len() - 1])) {
                Ok(pattern) => CredoTarget::Pattern(pattern),
                Err(_) => return,
            }
        } else {
            CredoTarget::Name(params.to_string())
        };
        self.credo.push(CredoDirective { lines, target });
    }

    fn credo_matches(target: &CredoTarget, name: Option<&str>) -> bool {
        let Some(name) = name else { return false };
        match target {
            CredoTarget::All => true,
            CredoTarget::Name(n) => n == name,
            CredoTarget::Pattern(p) => p.is_match(&format!("Elixir.{name}")),
        }
    }

    pub fn file_disabled(&self, id: &str, credo: Option<&str>) -> bool {
        self.file.contains("all")
            || self.file.contains(id)
            || self
                .credo
                .iter()
                .any(|d| d.lines.is_none() && Self::credo_matches(&d.target, credo))
    }

    pub fn line_disabled(&self, id: &str, credo: Option<&str>, line: usize) -> bool {
        self.lines
            .get(&line)
            .is_some_and(|ids| ids.contains("all") || ids.contains(id))
            || self.credo.iter().any(|d| {
                d.lines
                    .is_some_and(|(a, b)| (a..=b).contains(&(line as i64)))
                    && Self::credo_matches(&d.target, credo)
            })
    }
}
