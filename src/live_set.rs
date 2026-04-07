//! The "live set" — the union of crate-name-style identifiers that the
//! current project considers in-bounds. Anything in `target/.../` whose
//! name prefix isn't in this set is fair game for the sweeper.
//!
//! Sources:
//!   * every `package.name` reachable through `cargo metadata` (i.e. the
//!     transitive Cargo.lock graph)
//!   * every `target.name` belonging to a workspace member (bin, lib,
//!     example, test, bench) — these can differ from the package name and
//!     are what cargo uses as the unit prefix on disk
//!   * the literal `"build_script_build"`, which is the prefix cargo uses
//!     in `incremental/` for build-script units regardless of crate

use anyhow::{Context, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use std::collections::HashSet;
use std::path::Path;

use crate::inventory::to_snake;

/// One workspace target as printed by `--list-targets`.
#[derive(Debug, Clone)]
pub struct WorkspaceTarget {
    pub name: String,
    pub kind: String,
    pub package: String,
}

/// Run `cargo metadata` once and return all workspace member targets.
/// Used by `--list-targets`. Does NOT invoke the compiler — just resolves
/// Cargo.lock and reads manifests.
pub fn workspace_targets(manifest_path: &Path) -> Result<Vec<WorkspaceTarget>> {
    let metadata = run_cargo_metadata(manifest_path)?;
    let mut out = Vec::new();
    for member_id in &metadata.workspace_members {
        if let Some(pkg) = metadata.packages.iter().find(|p| &p.id == member_id) {
            for target in &pkg.targets {
                let kind = target
                    .kind
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                // Skip cargo's internal "custom-build" units (build scripts).
                // They're not useful as `--prune-target` arguments — they're
                // bookkeeping that exists for any crate with a build.rs.
                if kind == "custom-build" {
                    continue;
                }
                out.push(WorkspaceTarget {
                    name: target.name.clone(),
                    kind,
                    package: pkg.name.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    Ok(out)
}

fn run_cargo_metadata(manifest_path: &Path) -> Result<Metadata> {
    // Try --offline first (fast, no network) then fall back if it fails
    // because the user has uncached deps.
    MetadataCommand::new()
        .manifest_path(manifest_path)
        .other_options(vec!["--offline".to_string()])
        .exec()
        .or_else(|_| MetadataCommand::new().manifest_path(manifest_path).exec())
        .with_context(|| format!("running cargo metadata for {}", manifest_path.display()))
}

/// Names the project considers in-bounds. We hold both the original-cased
/// form (for matching `.fingerprint/` dir prefixes, which use package names
/// as-written) and the snake-cased form (for matching `deps/` filenames,
/// `incremental/` dir prefixes, and top-level workspace bin filenames).
#[derive(Debug, Clone)]
pub struct LiveSet {
    /// Original-case names: union of every package name AND every workspace
    /// member's target names (bin/lib/example/test/bench).
    crates: HashSet<String>,
    /// `crates` mapped through snake_case (`-` → `_`).
    snake_crates: HashSet<String>,
}

impl LiveSet {
    pub fn from_manifest(manifest_path: &Path) -> Result<Self> {
        let metadata = run_cargo_metadata(manifest_path)?;

        let mut crates: HashSet<String> = HashSet::new();
        for pkg in &metadata.packages {
            crates.insert(pkg.name.clone());
        }
        for member_id in &metadata.workspace_members {
            if let Some(pkg) = metadata.packages.iter().find(|p| &p.id == member_id) {
                for target in &pkg.targets {
                    crates.insert(target.name.clone());
                }
            }
        }
        // Cargo uses this literal as the dir prefix in incremental/ for
        // build-script units regardless of the parent crate.
        crates.insert("build_script_build".to_string());

        Ok(Self::from_set(crates))
    }

    fn from_set(crates: HashSet<String>) -> Self {
        let snake_crates = crates.iter().map(|s| to_snake(s)).collect();
        Self {
            crates,
            snake_crates,
        }
    }

    #[cfg(test)]
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut crates: HashSet<String> = names.into_iter().map(|s| s.into()).collect();
        crates.insert("build_script_build".to_string());
        Self::from_set(crates)
    }

    /// Match against original-case names (used for `.fingerprint/` prefixes).
    pub fn contains(&self, name: &str) -> bool {
        self.crates.contains(name)
    }

    /// Match against snake-cased names (used for `incremental/` prefixes
    /// and `deps/` filename roots).
    pub fn contains_snake(&self, name: &str) -> bool {
        self.snake_crates.contains(name)
    }

    /// Match either form. Used for top-level workspace artifacts whose
    /// filenames may use the original or the snake form depending on which
    /// kind of target produced them.
    pub fn matches_any(&self, name: &str) -> bool {
        self.crates.contains(name) || self.snake_crates.contains(name)
    }

    pub fn len(&self) -> usize {
        self.crates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.crates.is_empty()
    }
}
