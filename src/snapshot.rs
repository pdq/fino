use crate::{Finding, Result, Severity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

/// JSON Lines: one sorted, independently mergeable violation per line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub file: String,
    pub rule: String,
    /// Normalized source line, stored verbatim to avoid hash collisions.
    pub fingerprint: String,
    pub occurrence: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot(pub BTreeSet<Entry>);

pub(crate) fn valid_relative_path(file: &str) -> bool {
    !file.is_empty()
        && !file.contains('\\')
        && file
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl Snapshot {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read snapshot: {e}")),
        };
        let mut entries = BTreeSet::new();
        for (n, line) in text.lines().enumerate() {
            let entry: Entry =
                serde_json::from_str(line).map_err(|e| format!("snapshot line {}: {e}", n + 1))?;
            if !valid_relative_path(&entry.file) || entry.rule.is_empty() || entry.occurrence == 0 {
                return Err(format!("invalid snapshot entry on line {}", n + 1));
            }
            if !entries.insert(entry) {
                return Err(format!("duplicate snapshot entry on line {}", n + 1));
            }
        }
        Ok(Self(entries))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("refusing to replace a symlink snapshot".into());
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let temp = path.with_extension(format!("jsonl.{}.{stamp}.tmp", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("cannot create snapshot temporary file: {e}"))?;
        let mut file = BufWriter::new(file);
        for entry in &self.0 {
            writeln!(
                file,
                "{}",
                serde_json::to_string(entry).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
        }
        file.flush().map_err(|e| e.to_string())?;
        file.get_ref().sync_all().map_err(|e| e.to_string())?;
        fs::rename(temp, path).map_err(|e| format!("cannot replace snapshot: {e}"))
    }

    /// Hold a stable companion lock across policy validation and atomic replacement.
    pub fn write_update(
        &self,
        path: &Path,
        stale: &[Entry],
        current: &BTreeSet<Entry>,
        adopt: bool,
    ) -> Result<()> {
        let guard = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("jsonl.guard"))
            .map_err(|e| e.to_string())?;
        guard.lock().map_err(|e| e.to_string())?;
        let mut updated = Self::load(path)?;
        if updated.0 != self.0 {
            return Err("snapshot changed during scan; retry".into());
        }
        for entry in stale {
            updated.0.remove(entry);
        }
        if adopt {
            updated.0.extend(current.iter().cloned());
        }
        updated.save(path)
    }
}

pub(crate) fn entries(rows: &[Finding], text: &str) -> BTreeSet<Entry> {
    let lines: Vec<_> = text.split('\n').collect();
    let mut counts = HashMap::new();
    rows.iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| {
            let fingerprint = lines
                .get(f.line.saturating_sub(1))
                .unwrap_or(&"")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let occurrence = counts
                .entry((f.rule.clone(), fingerprint.clone()))
                .or_insert(0);
            *occurrence += 1;
            Entry {
                file: f.file.clone(),
                rule: f.rule.clone(),
                fingerprint,
                occurrence: *occurrence,
            }
        })
        .collect()
}
