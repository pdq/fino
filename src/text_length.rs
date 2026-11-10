//! Inclusive text-length bounds, independent of rendering width and lint policy.
//!
//! Input is measured exactly as provided: no trimming, normalization or line
//! splitting. CRLF is two bytes/scalars but one extended grapheme cluster; lone
//! CR and LF each count as one. Graphemes follow UAX #29 via unicode-segmentation.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LengthUnit {
    Bytes,
    #[default]
    Scalars,
    Graphemes,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextLength {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
    #[serde(default)]
    pub unit: LengthUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LengthError {
    NoBounds,
    InvertedBounds,
}

impl fmt::Display for LengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoBounds => "text length requires a minimum or maximum",
            Self::InvertedBounds => "text length minimum must not exceed maximum",
        })
    }
}
impl std::error::Error for LengthError {}

#[derive(Debug, Clone, Copy)]
pub struct CompiledTextLength {
    min: usize,
    max: Option<usize>,
    unit: LengthUnit,
}

pub fn compile(spec: &TextLength) -> Result<CompiledTextLength, LengthError> {
    if spec.min.is_none() && spec.max.is_none() {
        return Err(LengthError::NoBounds);
    }
    let min = spec.min.unwrap_or(0);
    if spec.max.is_some_and(|max| min > max) {
        return Err(LengthError::InvertedBounds);
    }
    Ok(CompiledTextLength {
        min,
        max: spec.max,
        unit: spec.unit,
    })
}

impl CompiledTextLength {
    /// Only the selected unit is evaluated. Iterators stop as soon as bounds
    /// permit a decision; no intermediate grapheme/character vectors are built.
    pub fn matches(&self, text: &str) -> bool {
        if self.min > text.len() {
            return false;
        }
        match self.unit {
            LengthUnit::Bytes => self.matches_count(text.len()),
            LengthUnit::Scalars => self.matches_iter(text.chars(), text.len()),
            LengthUnit::Graphemes if text.is_ascii() => {
                // UAX #29 joins CR+LF even in ASCII; every other ASCII byte is
                // its own grapheme, including controls and horizontal spacing.
                let crlf = text
                    .as_bytes()
                    .windows(2)
                    .filter(|pair| *pair == b"\r\n")
                    .count();
                self.matches_count(text.len() - crlf)
            }
            LengthUnit::Graphemes => self.matches_iter(text.graphemes(true), text.len()),
        }
    }

    fn matches_count(&self, count: usize) -> bool {
        count >= self.min && self.max.is_none_or(|max| count <= max)
    }

    fn matches_iter(&self, mut iter: impl Iterator, byte_len: usize) -> bool {
        // Every scalar or grapheme consumes at least one byte. This also avoids
        // overflow when the configured maximum is usize::MAX.
        if let Some(max) = self.max.filter(|&max| max < byte_len) {
            return self.matches_count(iter.take(max + 1).count());
        }
        self.min == 0 || iter.nth(self.min - 1).is_some()
    }
}
