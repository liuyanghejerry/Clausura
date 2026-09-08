//! Shard planning, bisection, and manifest building for diff-based audits.
//!
//! A large PR is split into bounded shards (groups of per-file diffs) so a
//! single agent run can always finish within its token/iteration budget.
//! When a shard still runs out of budget, it is bisected — by file list, or
//! by hunk boundaries for a single oversized file — and retried, instead of
//! re-billing the same oversized context.

use crate::risk::{tags_for_file, RiskHit};
use serde::{Deserialize, Serialize};

/// One file's unified diff with planning metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Workspace-relative path.
    pub path: String,
    /// Unified diff text (produced with `--unified=<context_lines>`).
    pub diff: String,
    /// Byte length of `diff`, cached for shard planning.
    pub diff_bytes: usize,
    /// First and last added line in the new file version; `None` when the
    /// change is deletions-only.
    pub changed_lines: Option<(u32, u32)>,
}

impl FileDiff {
    pub fn new(path: impl Into<String>, diff: impl Into<String>) -> Self {
        let diff = diff.into();
        let diff_bytes = diff.len();
        let changed_lines = extract_changed_lines(&diff);
        FileDiff {
            path: path.into(),
            diff,
            diff_bytes,
            changed_lines,
        }
    }
}

/// A group of files reviewed together as one bounded agent task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shard {
    pub files: Vec<FileDiff>,
}

impl Shard {
    pub fn total_bytes(&self) -> usize {
        self.files.iter().map(|f| f.diff_bytes).sum()
    }

    pub fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.path.as_str()).collect()
    }
}

/// Greedy planner: pack files into shards in input order until a shard
/// would exceed `max_diff_bytes` or `max_files_per_shard`, then start a new
/// one. A single file whose diff alone exceeds the byte budget still gets
/// its own shard — the orchestrator bisects it by hunks if it fails.
pub fn plan_shards(
    files: Vec<FileDiff>,
    max_diff_bytes: usize,
    max_files_per_shard: usize,
) -> Vec<Shard> {
    let mut shards: Vec<Shard> = Vec::new();
    let mut current = Shard { files: Vec::new() };
    let mut current_bytes = 0usize;

    for file in files {
        let fits_bytes =
            current_bytes + file.diff_bytes <= max_diff_bytes || current.files.is_empty();
        let fits_files = current.files.len() < max_files_per_shard;
        if !fits_bytes || !fits_files {
            if !current.files.is_empty() {
                shards.push(current);
            }
            current = Shard { files: Vec::new() };
            current_bytes = 0;
        }
        current_bytes += file.diff_bytes;
        current.files.push(file);
    }
    if !current.files.is_empty() {
        shards.push(current);
    }
    shards
}

/// Split a shard for a smaller retry.
///
/// Multi-file shards split by file list (order-preserving halves); a
/// single-file shard splits its diff by hunk boundaries. Returns `None`
/// when no further split is possible (one file, one hunk).
pub fn bisect_shard(shard: &Shard) -> Option<(Shard, Shard)> {
    if shard.files.len() > 1 {
        let mid = shard.files.len().div_ceil(2);
        let mut left = shard.files.clone();
        let right = left.split_off(mid);
        return Some((Shard { files: left }, Shard { files: right }));
    }
    let file = shard.files.first()?;
    let (l, r) = bisect_file_hunks(file)?;
    Some((Shard { files: vec![l] }, Shard { files: vec![r] }))
}

/// Split one file's diff at a hunk boundary near the byte midpoint. The
/// preamble (`diff --git`, `index`, `---`, `+++` lines) is duplicated into
/// both halves so each remains a valid unified diff. Returns `None` when
/// the diff has fewer than two hunks.
fn bisect_file_hunks(file: &FileDiff) -> Option<(FileDiff, FileDiff)> {
    let mut preamble = String::new();
    let mut hunks: Vec<String> = Vec::new();
    for line in file.diff.lines() {
        if line.starts_with("@@") {
            hunks.push(String::new());
        }
        match hunks.last_mut() {
            Some(cur) => {
                cur.push_str(line);
                cur.push('\n');
            }
            None => {
                preamble.push_str(line);
                preamble.push('\n');
            }
        }
    }
    if hunks.len() < 2 {
        return None;
    }

    let total: usize = hunks.iter().map(|h| h.len()).sum();
    let mut accumulated = 0usize;
    let mut split_at = hunks.len() / 2;
    for (i, hunk) in hunks.iter().enumerate() {
        accumulated += hunk.len();
        if accumulated >= total / 2 {
            split_at = i + 1;
            break;
        }
    }
    // Both halves must keep at least one hunk; clamp even when the last
    // hunk is what crossed the midpoint.
    split_at = split_at.clamp(1, hunks.len() - 1);

    let left: String = hunks[..split_at].concat();
    let right: String = hunks[split_at..].concat();
    let left_file = FileDiff::new(file.path.clone(), preamble.clone() + &left);
    let right_file = FileDiff::new(file.path.clone(), preamble + &right);
    Some((left_file, right_file))
}

/// Build the shard manifest for the agent's initial message: per-file
/// metadata plus the deterministic pre-scan hotspots.
pub fn build_manifest(shard: &Shard, risk_hits: &[RiskHit]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "SHARD: {} file(s), {} diff bytes total",
        shard.files.len(),
        shard.total_bytes()
    );

    for file in &shard.files {
        let _ = writeln!(out, "FILE: {}", file.path);
        match file.changed_lines {
            Some((first, last)) => {
                let _ = writeln!(out, "CHANGED_LINES: {first}-{last}");
            }
            None => {
                let _ = writeln!(out, "CHANGED_LINES: (deletions only)");
            }
        }
        let _ = writeln!(out, "DIFF_BYTES: {}", file.diff_bytes);
        let tags = tags_for_file(risk_hits, &file.path);
        if tags.is_empty() {
            let _ = writeln!(out, "RISK_TAGS: (none)");
        } else {
            let _ = writeln!(out, "RISK_TAGS: {tags}");
        }
    }

    let shard_paths: Vec<&str> = shard.paths();
    let hotspots: Vec<&RiskHit> = risk_hits
        .iter()
        .filter(|h| shard_paths.contains(&h.file.as_str()))
        .collect();
    if hotspots.is_empty() {
        out.push_str(
            "CANDIDATE HOTSPOTS: none flagged by the deterministic pre-scan — review the full diff.\n",
        );
    } else {
        out.push_str("CANDIDATE HOTSPOTS (deterministic pre-scan of added lines — start here):\n");
        for h in hotspots {
            let _ = writeln!(out, "- {}:{} [{}] {}", h.file, h.line, h.tag, h.snippet);
        }
    }
    out
}

/// Extract the min/max added line numbers from a unified diff.
fn extract_changed_lines(diff: &str) -> Option<(u32, u32)> {
    let mut new_line: u32 = 0;
    let mut in_hunk = false;
    let mut first: Option<u32> = None;
    let mut last: u32 = 0;

    for raw in diff.lines() {
        if raw.starts_with("@@") {
            new_line = parse_new_start(raw)?;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if raw.starts_with('+') {
            if first.is_none() {
                first = Some(new_line);
            }
            last = new_line;
            new_line += 1;
        } else if raw.starts_with(' ') || raw.is_empty() {
            new_line += 1;
        }
    }
    first.map(|f| (f, last))
}

fn parse_new_start(header: &str) -> Option<u32> {
    let token = header
        .split_whitespace()
        .find(|t| t.starts_with('+') && t.len() > 1)?;
    let start = token[1..].split(',').next()?;
    start.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a FileDiff whose total diff size is (within rounding) `bytes`.
    fn file(path: &str, bytes: usize) -> FileDiff {
        let overhead = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n+\n context\n+\n"
        )
        .len();
        let content = bytes.saturating_sub(overhead);
        let diff = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n+{}\n context\n+{}\n",
            "a".repeat(content / 2),
            "b".repeat(content / 2)
        );
        FileDiff::new(path, diff)
    }

    #[test]
    fn test_file_diff_metadata() {
        let f = FileDiff::new(
            "src/a.ts",
            "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -5,2 +10,4 @@\n ctx\n+new1\n+new2\n",
        );
        assert!(f.diff_bytes > 0);
        assert_eq!(f.changed_lines, Some((11, 12)));
    }

    #[test]
    fn test_changed_lines_deletions_only() {
        let f = FileDiff::new("src/b.ts", "@@ -3,2 +3,1 @@\n-deleted\n ctx\n");
        assert_eq!(f.changed_lines, None);
    }

    #[test]
    fn test_plan_shards_packs_under_byte_budget() {
        let files = vec![file("a.ts", 100), file("b.ts", 100), file("c.ts", 100)];
        let shards = plan_shards(files, 250, 10);
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].paths(), vec!["a.ts", "b.ts"]);
        assert_eq!(shards[1].paths(), vec!["c.ts"]);
    }

    #[test]
    fn test_plan_shards_respects_file_cap() {
        let files = vec![file("a.ts", 10), file("b.ts", 10), file("c.ts", 10)];
        let shards = plan_shards(files, 10_000, 2);
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].files.len(), 2);
        assert_eq!(shards[1].files.len(), 1);
    }

    #[test]
    fn test_plan_shards_oversized_single_file_gets_own_shard() {
        let files = vec![file("a.ts", 10), file("big.ts", 100_000), file("b.ts", 10)];
        let shards = plan_shards(files, 1000, 10);
        assert_eq!(
            shards.len(),
            3,
            "oversized file isolates into its own shard"
        );
        assert_eq!(shards[1].paths(), vec!["big.ts"]);
    }

    #[test]
    fn test_plan_shards_empty_input() {
        assert!(plan_shards(vec![], 100, 10).is_empty());
    }

    #[test]
    fn test_bisect_shard_multi_file_halves() {
        let files = vec![
            file("a.ts", 10),
            file("b.ts", 10),
            file("c.ts", 10),
            file("d.ts", 10),
        ];
        let shard = Shard { files };
        let (l, r) = bisect_shard(&shard).unwrap();
        assert_eq!(l.paths(), vec!["a.ts", "b.ts"]);
        assert_eq!(r.paths(), vec!["c.ts", "d.ts"]);
    }

    #[test]
    fn test_bisect_shard_single_file_by_hunks() {
        let diff = "diff --git a/big.ts b/big.ts\n--- a/big.ts\n+++ b/big.ts\n\
                    @@ -1,3 +1,3 @@\n+first hunk change\n ctx\n ctx\n\
                    @@ -20,3 +20,3 @@\n ctx\n+second hunk change\n ctx\n";
        let shard = Shard {
            files: vec![FileDiff::new("big.ts", diff)],
        };
        let (l, r) = bisect_shard(&shard).unwrap();
        assert_eq!(l.files.len(), 1);
        assert_eq!(r.files.len(), 1);
        assert_eq!(l.files[0].path, "big.ts");
        assert!(l.files[0].diff.contains("first hunk change"));
        assert!(!l.files[0].diff.contains("second hunk change"));
        assert!(r.files[0].diff.contains("second hunk change"));
        // Both halves keep the preamble, so each stays a valid diff.
        assert!(l.files[0].diff.contains("diff --git"));
        assert!(r.files[0].diff.contains("diff --git"));
    }

    #[test]
    fn test_bisect_shard_single_hunk_is_terminal() {
        let diff = "--- a/x.ts\n+++ b/x.ts\n@@ -1,9 +1,9 @@\n+a\n+b\n+c\n ctx\n ctx\n";
        let shard = Shard {
            files: vec![FileDiff::new("x.ts", diff)],
        };
        assert!(bisect_shard(&shard).is_none());
    }

    #[test]
    fn test_build_manifest_fields() {
        let shard = Shard {
            files: vec![file("services/data-manager.ts", 200)],
        };
        let hits = vec![RiskHit {
            tag: "filesystem".to_string(),
            file: "services/data-manager.ts".to_string(),
            line: 472,
            snippet: "fs.writeFile(path, data)".to_string(),
        }];
        let manifest = build_manifest(&shard, &hits);
        assert!(manifest.contains("SHARD: 1 file(s)"), "got: {manifest}");
        assert!(manifest.contains("FILE: services/data-manager.ts"));
        assert!(manifest.contains("CHANGED_LINES: "));
        assert!(manifest.contains("DIFF_BYTES: "));
        assert!(manifest.contains("RISK_TAGS: filesystem"));
        assert!(manifest.contains("CANDIDATE HOTSPOTS"));
        assert!(manifest.contains("- services/data-manager.ts:472 [filesystem] fs.writeFile"));
    }

    #[test]
    fn test_build_manifest_without_hits() {
        let shard = Shard {
            files: vec![file("a.ts", 50)],
        };
        let manifest = build_manifest(&shard, &[]);
        assert!(manifest.contains("RISK_TAGS: (none)"));
        assert!(manifest.contains("none flagged by the deterministic pre-scan"));
    }
}
