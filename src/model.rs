//! On-disk formats. These types are the single source of truth for parsing and
//! for the published JSON Schemas (`fino schema rule|config`).
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Project configuration, normally `.fino/config.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Root-relative globs of files to lint. Default: all `.ex` and `.exs` files.
    #[serde(default = "default_includes")]
    pub include: Vec<String>,
    /// Root-relative globs excluded from every rule. Directory globs also exclude their contents.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Directory containing one rule per `<id>.toml` file, relative to this config file.
    #[serde(default = "default_rules_dir")]
    pub rules_dir: String,
    /// Committable violation snapshot, relative to this config file.
    #[serde(default = "default_snapshot_file")]
    pub snapshot_file: String,
    /// Compatibility switches for migrating from other linters.
    #[serde(default)]
    pub compat: ProjectCompat,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCompat {
    /// Honor `# credo:disable-for-this-file`, `-for-next-line`, `-for-previous-line` and
    /// `-for-lines:N` comments for rules that declare `compat.credo`.
    #[serde(default)]
    pub credo_directives: bool,
}

pub fn default_includes() -> Vec<String> {
    vec!["**/*.ex".into(), "**/*.exs".into()]
}
fn default_rules_dir() -> String {
    "rules".into()
}
fn default_snapshot_file() -> String {
    "snapshot.jsonl".into()
}

/// One lint rule. Store each rule in `<rules_dir>/<id>.toml`.
///
/// Exactly one matcher is required: `query`, `regex` or `path_only`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleFile {
    /// Unique id: lowercase letters, digits and dashes. Must equal the file name without `.toml`.
    #[schemars(regex(pattern = r"^[a-z0-9]+(-[a-z0-9]+)*$"))]
    pub id: String,
    /// Message reported for each finding.
    pub message: String,
    /// Optional longer explanation: why the rule exists and how to fix findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// New `error` findings fail checks; `warning` findings are only reported.
    #[serde(default)]
    pub severity: Severity,
    /// Only lint files matching these root-relative globs (default: every configured file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    /// Skip files matching these root-relative globs. Directory globs also exclude their contents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// Tree-sitter query over the Elixir syntax tree. Each `@finding` capture is one finding.
    /// Run `fino tree FILE` to see node kinds and field names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Regular expression (Rust syntax plus look-around). Each match is one finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// Report one finding at line 1 of every selected file. Use with `include`/`exclude`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub path_only: bool,

    /// For `regex`: an optional fast regex (no look-around) checked on the raw, unmasked line
    /// (`scope = "line"`) or file before masking and `regex` run. Purely an optimization:
    /// it must match the raw text whenever `regex` matches the masked text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefilter: Option<String>,
    /// For `regex`: inclusive length bounds on each matched substring after masking.
    /// Measures bytes, Unicode scalars (default), or extended grapheme clusters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_length: Option<crate::text_length::TextLength>,
    /// For `regex`: match the whole file (default) or each line separately.
    #[serde(default)]
    pub scope: Scope,
    /// For `regex`: blank or remove syntax (strings, comments, ...) before matching.
    /// One mask, or a list of masks applied in order (later masks win).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Masks>,
    /// Drop findings on lines matching this condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_lines: Option<Condition>,
    /// Drop findings located inside nodes captured by this tree-sitter query
    /// (captures not starting with `_`), e.g. everything inside `quote do ... end`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_inside: Option<String>,
    /// Only lint files where this condition matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_file: Option<Condition>,
    /// Skip files where this condition matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_file: Option<Condition>,
    /// Limit findings per file. Applied before suppression comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    /// Names this rule has in other linters, for suppression-comment compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<RuleCompat>,

    /// Examples run by `fino test`. Include at least one finding and one clean case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<RuleTest>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    File,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Limit {
    /// Keep only the first finding in each file.
    FirstPerFile,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Masks {
    One(Mask),
    Many(Vec<Mask>),
}

impl Masks {
    pub fn layers(&self) -> &[Mask] {
        match self {
            Masks::One(mask) => std::slice::from_ref(mask),
            Masks::Many(masks) => masks,
        }
    }
}

/// Replace syntax content before regex matching. Newlines are always kept, so line
/// numbers do not change. Delimiters (quotes, `~r/`, ...) are kept.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    /// Syntax to mask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<MaskNode>,
    /// Tree-sitter query selecting more nodes to mask (captures not starting with `_`).
    /// Quoted nodes (strings, sigils, ...) mask their content; other nodes mask entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Replacement for each masked byte: one ASCII character, or "" to remove the content.
    #[serde(default = "default_mask_with")]
    pub with: String,
    /// Also put `with` on empty lines inside masked content, so they no longer look blank.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill_blank_lines: bool,
}

fn default_mask_with() -> String {
    " ".into()
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum MaskNode {
    /// Content of "strings" and """heredocs""", including interpolations.
    String,
    /// Content of 'charlists' and '''charlist heredocs'''.
    Charlist,
    /// Content of sigils such as ~r/.../ and ~s(...).
    Sigil,
    /// Content of quoted atoms (:"a b") and quoted keyword keys ("a b": 1).
    QuotedAtom,
    /// Entire comments, including the `#`.
    Comment,
}

/// A condition matches when any given matcher matches. At least one is required.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// Regex. For `skip_lines` it runs on each (masked) line; otherwise on the raw file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// Tree-sitter query. For `skip_lines`, lines where a non-`_` capture starts match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleCompat {
    /// Fully qualified Credo check name, e.g. `Credo.Check.Warning.IoInspect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credo: Option<String>,
}

/// An example source file and the lines where the rule must report findings.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleTest {
    /// What the example demonstrates.
    pub name: String,
    /// Path the example pretends to have; matters for `include`/`exclude`.
    #[serde(default = "default_test_path")]
    pub path: String,
    /// Elixir source to lint.
    pub code: String,
    /// One-based line of every expected finding, repeated for several findings on one line.
    /// Use `[]` for a clean example.
    pub expect: Vec<usize>,
}

fn default_test_path() -> String {
    "lib/example.ex".into()
}
