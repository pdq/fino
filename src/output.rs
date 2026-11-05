//! Human diagnostics only. JSON serialization deliberately bypasses this module.
use crate::cli::Args;
use fino::{Engine, Finding, Report, Severity, model::RuleFile};
use std::collections::{BTreeMap, VecDeque};
use std::io::IsTerminal;
use std::{fmt::Write, fs};
use unicode_width::UnicodeWidthStr;

/// Never allow policy text or file names to inject terminal commands or new rows.
pub fn safe(text: &str) -> String {
    let mut rendered = String::new();
    for c in text.chars() {
        let hidden = matches!(c,
            '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}' | '\u{200e}' | '\u{200f}'
            | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e007f}');
        if c.is_control() || hidden {
            rendered.extend(c.escape_default());
        } else {
            rendered.push(c);
        }
    }
    rendered
}

fn colored(text: &str, code: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.into()
    }
}

fn color(args: &Args, terminal: bool) -> bool {
    match args.color.as_str() {
        "always" => true,
        "never" => false,
        _ => terminal && std::env::var_os("NO_COLOR").is_none(),
    }
}

struct Source {
    text: String,
    starts: Vec<usize>,
}

const SOURCE_CACHE_BYTES: usize = 4 * 1024 * 1024;
const GROUPED_CACHE_FILES: usize = 8;

struct CachedSource<'a> {
    file: &'a str,
    source: Option<Source>,
    bytes: usize,
}

/// Most recently used source is first. Limits count source/index allocations,
/// not borrowed file names or the small, file-count-bounded entry overhead.
/// A source larger than the budget is retained alone, only while current; it is
/// dropped before loading the next file. Failed reads are cached within the same
/// file-count limit so adjacent findings do not retry an unavailable file.
struct SourceCache<'a> {
    entries: VecDeque<CachedSource<'a>>,
    bytes: usize,
    max_files: usize,
    max_bytes: usize,
}

impl<'a> SourceCache<'a> {
    fn new(max_files: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_files,
            max_bytes,
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_back() {
            self.bytes -= entry.bytes;
        }
    }

    fn get(&mut self, file: &'a str, load: impl FnOnce() -> Option<Source>) -> Option<&Source> {
        if let Some(index) = self.entries.iter().position(|entry| entry.file == file) {
            let entry = self.entries.remove(index).expect("existing cache entry");
            self.entries.push_front(entry);
        } else {
            // Release a completed oversized source, or the single-file cache,
            // before reading the next file rather than retaining two large files.
            if self.max_files == 1 || self.bytes > self.max_bytes {
                self.entries.clear();
                self.bytes = 0;
            }
            let source = load();
            let bytes = source.as_ref().map_or(0, Source::allocated_bytes);
            while !self.entries.is_empty()
                && (self.entries.len() >= self.max_files
                    || self.bytes.saturating_add(bytes) > self.max_bytes)
            {
                self.evict_oldest();
            }
            self.entries.push_front(CachedSource {
                file,
                source,
                bytes,
            });
            self.bytes += bytes;
        }
        self.entries.front().and_then(|entry| entry.source.as_ref())
    }
}

impl Source {
    fn allocated_bytes(&self) -> usize {
        self.text
            .capacity()
            .saturating_add(self.starts.capacity().saturating_mul(size_of::<usize>()))
    }

    fn read(path: &std::path::Path) -> Option<Self> {
        fs::read_to_string(path).ok().map(Self::new)
    }

    fn new(text: String) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    fn snippet(&self, row: &Finding, context: usize, color: bool) -> String {
        if context == 0
            || row.start_byte > self.text.len()
            || row.end_byte > self.text.len()
            || row.end_byte < row.start_byte
            || !self.text.is_char_boundary(row.start_byte)
            || !self.text.is_char_boundary(row.end_byte)
        {
            return String::new();
        }
        let first = self
            .starts
            .partition_point(|&start| start <= row.start_byte)
            .saturating_sub(1);
        let last_byte = row.end_byte.saturating_sub(1).max(row.start_byte);
        let last = self
            .starts
            .partition_point(|&start| start <= last_byte)
            .saturating_sub(1);
        let low = first.saturating_sub(context);
        let high = last.saturating_add(context).min(self.starts.len() - 1);
        let width = (high + 1).to_string().len();
        let mut result = String::new();
        for index in low..=high {
            let start = self.starts[index];
            if start == self.text.len() {
                continue;
            }
            let end = self
                .starts
                .get(index + 1)
                .copied()
                .unwrap_or(self.text.len());
            let line = self.text[start..end].trim_end_matches(['\n', '\r']);
            let rendered = source_text(line);
            let _ = writeln!(result, "  {:>width$} | {rendered}", index + 1);
            if (first..=last).contains(&index) {
                let from = row.start_byte.saturating_sub(start).min(line.len());
                let to = row.end_byte.saturating_sub(start).min(line.len());
                // Newline-only matches point at the end of the preceding line.
                let prefix = source_text(&line[..from]).width();
                let length = source_text(&line[..to.max(from)])
                    .width()
                    .saturating_sub(prefix)
                    .max(1);
                let marker = format!("{}{}", " ".repeat(prefix), "^".repeat(length));
                let _ = writeln!(
                    result,
                    "  {:width$} | {}",
                    "",
                    colored(&marker, "36", color)
                );
            }
        }
        result
    }
}

// Measure whole strings, not individual characters: combining marks and emoji
// sequences affect width. Preserve joiners needed for ordinary scripts and emoji.
// Tabs advance to four-column stops in the rendered source (excluding the gutter).
fn source_text(text: &str) -> String {
    source_text_with_width(text, |part| part.width())
}

fn source_text_with_width(text: &str, mut width: impl FnMut(&str) -> usize) -> String {
    let mut rendered = String::new();
    let mut column = 0;
    for (index, part) in text.split('\t').enumerate() {
        if index > 0 {
            let spaces = 4 - column % 4;
            rendered.push_str(&" ".repeat(spaces));
            column += spaces;
        }
        let escaped = safe(part);
        // Tabs are sequence boundaries. Measure each whole escaped segment once
        // to preserve combining/ZWJ behavior without rescanning growing prefixes.
        column += width(&escaped);
        rendered.push_str(&escaped);
    }
    rendered
}

pub fn human(report: &Report, engine: &Engine, args: &Args) {
    let stdout_color = color(args, std::io::stdout().is_terminal());
    let stderr_color = color(args, std::io::stderr().is_terminal());
    let mut sources = SourceCache::new(
        if args.group_by == "rule" {
            GROUPED_CACHE_FILES
        } else {
            1
        },
        SOURCE_CACHE_BYTES,
    );
    let mut groups: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for row in &report.findings {
        groups
            .entry(if args.group_by == "rule" {
                &row.rule
            } else {
                ""
            })
            .or_default()
            .push(row);
    }
    for (rule_id, rows) in groups {
        if args.group_by == "rule" {
            let rule = engine.rules.iter().find(|rule| rule.id == rule_id);
            let description = rule
                .and_then(|rule| fs::read_to_string(&rule.file).ok())
                .and_then(|text| toml::from_str::<RuleFile>(&text).ok())
                .and_then(|rule| rule.description)
                .filter(|text| !text.trim().is_empty());
            let fallback = rule.map_or(rows[0].message.as_str(), |rule| rule.message.as_str());
            println!(
                "\n{} — {}",
                colored(&safe(rule_id), "1;36", stdout_color),
                safe(description.as_deref().unwrap_or(fallback))
            );
        }
        for row in rows {
            let (severity, code) = if row.severity == Severity::Warning {
                ("warning", "33")
            } else {
                ("error", "31")
            };
            println!(
                "{}:{}:{}: {} [{}]: {}",
                safe(&row.file),
                row.line,
                row.column,
                colored(severity, code, stdout_color),
                colored(&safe(&row.rule), "1", stdout_color),
                safe(&row.message)
            );
            if args.context > 0
                && let Some(source) =
                    sources.get(&row.file, || Source::read(&args.root.join(&row.file)))
            {
                print!("{}", source.snippet(row, args.context, stdout_color));
            }
        }
    }
    for error in &report.errors {
        eprintln!(
            "{}:{}:{}: {}: {}",
            safe(&error.file),
            error.line,
            error.column,
            colored("error", "31", stderr_color),
            safe(&error.message)
        );
    }
    for entry in &report.stale {
        eprintln!(
            "{}: {}: {} (run fino snapshot --update)",
            safe(&entry.file),
            safe(&entry.rule),
            colored("stale snapshot entry", "33", stderr_color)
        );
    }
    println!(
        "{}: {} files, {} rules, {} findings, {} errors, {:.2} ms ({})",
        colored(
            if report.passed { "PASS" } else { "FAIL" },
            if report.passed { "32" } else { "31" },
            stdout_color
        ),
        report.files,
        engine.rules.len(),
        report.findings.len(),
        report.errors.len(),
        report.elapsed_ms,
        safe(&report.scope)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(start_byte: usize, end_byte: usize) -> Finding {
        Finding {
            file: "example.ex".into(),
            line: 1,
            column: start_byte + 1,
            rule: "example-rule".into(),
            message: "Example".into(),
            severity: Severity::Error,
            start_byte,
            end_byte,
        }
    }

    #[test]
    fn snippets_align_after_unicode_and_tabs_and_reject_invalid_offsets() {
        let source = Source::new("é\tcall()\n".into());
        let snippet = source.snippet(&finding(3, 7), 1, false);
        // The accented character takes one cell; the tab advances to column four.
        assert!(snippet.contains("é   call()"), "{snippet}");
        assert!(snippet.contains("|     ^^^^"), "{snippet}");
        assert!(source.snippet(&finding(1, 7), 1, false).is_empty());
        assert!(source.snippet(&finding(0, 100), 1, false).is_empty());
        assert!(source.snippet(&finding(3, 7), 0, false).is_empty());
    }

    #[test]
    fn unicode_sequences_set_prefix_and_highlight_widths() {
        for (prefix, cells) in [
            ("é", 1),
            ("界", 2),
            ("e\u{301}", 1),
            ("👩\u{200d}💻", 2),
            ("é界e\u{301}👩\u{200d}💻", 6),
        ] {
            let source = Source::new(format!("{prefix}call()\n"));
            let snippet = source.snippet(&finding(prefix.len(), prefix.len() + 4), 1, false);
            assert!(snippet.contains(&format!("| {prefix}call()")), "{snippet}");
            let marker = snippet.lines().find(|line| line.contains('^')).unwrap();
            assert_eq!(marker, format!("    | {}^^^^", " ".repeat(cells)));

            let snippet = source.snippet(&finding(0, prefix.len()), 1, false);
            let marker = snippet.lines().find(|line| line.contains('^')).unwrap();
            assert_eq!(marker, format!("    | {}", "^".repeat(cells)));
        }
        assert_eq!(
            source_text("界\te\u{301}\t👩\u{200d}💻\tx"),
            "界  e\u{301}   👩\u{200d}💻  x"
        );
    }

    #[test]
    fn escaped_controls_use_rendered_cell_widths_for_carets() {
        let prefix = "é\u{1b}[31m\u{202e}\u{200b}\u{2060}";
        let rendered = "é\\u{1b}[31m\\u{202e}\\u{200b}\\u{2060}";
        assert_eq!(source_text(prefix), rendered);
        let source = Source::new(format!("{prefix}call()\n"));
        let snippet = source.snippet(&finding(prefix.len(), prefix.len() + 4), 1, false);
        assert!(!snippet.contains('\x1b'));
        let marker = snippet.lines().find(|line| line.contains('^')).unwrap();
        assert_eq!(
            marker,
            format!("    | {}^^^^", " ".repeat(rendered.width()))
        );
    }

    #[test]
    fn unavailable_source_is_optional_and_controls_are_escaped() {
        // A child path of a regular source file cannot be opened as a file.
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/output.rs/missing.ex");
        assert!(Source::read(&path).is_none());
        assert_eq!(safe("a\x1b[31m\r\n\u{202e}"), "a\\u{1b}[31m\\r\\n\\u{202e}");
    }

    #[test]
    fn default_cache_retains_only_current_file_and_does_not_reread_it() {
        let mut cache = SourceCache::new(1, SOURCE_CACHE_BYTES);
        let mut reads = 0;
        for file in ["first.ex", "first.ex", "second.ex", "second.ex", "third.ex"] {
            let source = cache
                .get(file, || {
                    reads += 1;
                    Some(Source::new(file.into()))
                })
                .unwrap();
            assert_eq!(source.text, file);
            assert_eq!(cache.entries.len(), 1);
            assert_eq!(cache.entries[0].file, file);
            assert!(cache.bytes <= SOURCE_CACHE_BYTES);
        }
        assert_eq!(reads, 3);
        cache.get("missing.ex", || None);
        assert!(
            cache
                .get("missing.ex", || panic!(
                    "must not retry current failed read"
                ))
                .is_none()
        );
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn grouped_cache_evicts_least_recently_used_files_at_file_limit() {
        let mut cache = SourceCache::new(GROUPED_CACHE_FILES, SOURCE_CACHE_BYTES);
        let files: Vec<_> = (0..GROUPED_CACHE_FILES + 1)
            .map(|i| format!("file-{i}.ex"))
            .collect();
        for file in files.iter().take(GROUPED_CACHE_FILES) {
            cache.get(file, || Some(Source::new(file.clone())));
        }
        cache.get(&files[0], || panic!("recent source must be reused"));
        cache.get(&files[GROUPED_CACHE_FILES], || {
            Some(Source::new("next".into()))
        });
        assert_eq!(cache.entries.len(), GROUPED_CACHE_FILES);
        assert!(cache.entries.iter().any(|entry| entry.file == files[0]));
        assert!(!cache.entries.iter().any(|entry| entry.file == files[1]));
        let mut reread = false;
        cache.get(&files[1], || {
            reread = true;
            Some(Source::new("reread".into()))
        });
        assert!(reread);
        assert_eq!(cache.entries.len(), GROUPED_CACHE_FILES);
        assert!(cache.bytes <= SOURCE_CACHE_BYTES);
    }

    #[test]
    fn cache_budget_counts_indexes_and_oversized_sources_are_current_only() {
        let source = || Source::new("\n".repeat(32));
        let bytes = source().allocated_bytes();
        // Dense line indexes count too, not just source text.
        assert!(bytes > 32);
        let mut cache = SourceCache::new(GROUPED_CACHE_FILES, bytes * 2);
        for file in ["a.ex", "b.ex", "c.ex"] {
            cache.get(file, || Some(source()));
            assert!(cache.bytes <= bytes * 2);
        }
        assert_eq!(cache.entries.len(), 2);
        assert!(!cache.entries.iter().any(|entry| entry.file == "a.ex"));
        let large = || Source::new("x".repeat(bytes * 3));
        cache.get("large.ex", || Some(large()));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.bytes > cache.max_bytes);
        cache.get("large.ex", || {
            panic!("adjacent findings must reuse large current source")
        });
        cache.get("small.ex", || Some(source()));
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.bytes <= cache.max_bytes);
        assert!(!cache.entries.iter().any(|entry| entry.file == "large.ex"));
        let mut reread = false;
        cache.get("large.ex", || {
            reread = true;
            Some(large())
        });
        assert!(reread, "oversized files must not survive a file switch");
    }

    #[test]
    fn tab_rich_rendering_measures_each_escaped_segment_once() {
        let segment = "é界e\u{301}👩\u{200d}💻\x1b";
        let escaped = "é界e\u{301}👩\u{200d}💻\\u{1b}";
        let count = 4096;
        let source = format!("{}end", format!("{segment}\t").repeat(count));
        let mut width_calls = 0;
        let mut measured_bytes = 0;
        let rendered = source_text_with_width(&source, |part| {
            width_calls += 1;
            measured_bytes += part.len();
            assert!(!part.contains('\t') && !part.contains('\x1b'));
            assert!(part == escaped || part == "end");
            part.width()
        });
        assert_eq!(width_calls, count + 1);
        assert_eq!(measured_bytes, escaped.len() * count + 3);
        // Six literal cells plus six escaped cells, then a four-space tab stop.
        assert_eq!(
            rendered,
            format!("{}end", format!("{escaped}    ").repeat(count))
        );
    }
}
