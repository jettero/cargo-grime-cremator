//! The actual cleanup pass. Given a target dir + a live set, removes
//! anything cargo no longer references.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use std::collections::HashSet;

use crate::inventory::{extract_deps_hash, parse_unit_dirname, to_snake, Inventory};
use crate::live_set::LiveSet;

#[derive(Debug, Default, Clone, Copy)]
pub struct SweepReport {
    pub removed_files: usize,
    pub removed_dirs: usize,
    pub freed_bytes: u64,
}

impl SweepReport {
    pub fn merge(&mut self, other: &SweepReport) {
        self.removed_files += other.removed_files;
        self.removed_dirs += other.removed_dirs;
        self.freed_bytes += other.freed_bytes;
    }

    pub fn total_removed(&self) -> usize {
        self.removed_files + self.removed_dirs
    }
}

#[derive(Debug, Clone, Default)]
pub struct SweepOptions {
    pub dry_run: bool,
    pub keep_sessions: usize,
    pub verbose: bool,
    /// If Some, restrict the regular sweep to a single profile name
    /// (e.g. "debug"). Doesn't affect explicit `--prune-*` flags.
    pub only_profile: Option<String>,
    /// If Some, drop fingerprint dirs whose `invoked.timestamp` is older
    /// than this absolute cutoff. See `Inventory::build` for semantics.
    pub max_age_cutoff: Option<std::time::SystemTime>,
    /// `--prune-profile` — wipe these profile dirs entirely.
    pub prune_profiles: Vec<String>,
    /// `--prune-target` — wipe all artifacts for these workspace targets
    /// across every profile dir.
    pub prune_targets: Vec<String>,
}

pub fn sweep_target_dir(
    target_dir: &Path,
    live: &LiveSet,
    opts: &SweepOptions,
) -> Result<SweepReport> {
    let mut report = SweepReport::default();
    if !target_dir.is_dir() {
        return Ok(report);
    }

    // 1. Explicit --prune-profile: wipe whole profile dirs first.
    for prof in &opts.prune_profiles {
        let p = target_dir.join(prof);
        if p.is_dir() {
            if opts.verbose {
                println!("prune-profile: {}", p.display());
            }
            remove_dir(&p, opts, &mut report)?;
        }
    }

    // 2. Regular sweep + --prune-target across each surviving profile dir.
    for entry in std::fs::read_dir(target_dir)
        .with_context(|| format!("reading {}", target_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // skip cargo-internal & non-profile dirs
        if name_str.starts_with('.')
            || name_str == "doc"
            || name_str == "package"
            || name_str == "tmp"
        {
            continue;
        }
        if let Some(only) = &opts.only_profile {
            if name_str.as_ref() != only.as_str() {
                continue;
            }
        }
        // A profile dir always has a `.fingerprint/` subdir.
        if !path.join(".fingerprint").is_dir() {
            continue;
        }
        if opts.verbose {
            println!("profile: {}", name_str);
        }
        let prof_report = sweep_profile(&path, live, opts)?;
        report.merge(&prof_report);
    }
    Ok(report)
}

fn sweep_profile(profile_dir: &Path, live: &LiveSet, opts: &SweepOptions) -> Result<SweepReport> {
    let mut inv = Inventory::build(profile_dir, live, opts.max_age_cutoff)?;
    let mut report = SweepReport::default();

    // Apply --prune-target by promoting matching live fingerprint hashes
    // to "stale". Cascades through deps/build/incremental/top-level
    // automatically because everything keys off `inv.live_hashes`.
    if !opts.prune_targets.is_empty() {
        promote_pruned_targets_to_stale(profile_dir, &mut inv, &opts.prune_targets)?;
    }

    sweep_deps(profile_dir, &inv, opts, &mut report)?;
    sweep_build(profile_dir, &inv, opts, &mut report)?;
    sweep_incremental(profile_dir, live, opts, &opts.prune_targets, &mut report)?;
    sweep_top_level(profile_dir, &inv, live, opts, &mut report)?;
    sweep_examples(profile_dir, &inv, live, opts, &mut report)?;

    // Stale fingerprint dirs are removed last so they remain available for
    // inspection during the inventory phase.
    for p in &inv.stale_fingerprints {
        remove_dir(p, opts, &mut report)?;
    }
    Ok(report)
}

/// Walk `<profile>/.fingerprint/` again and demote any matching
/// `<crate>-<HASH>/` to the stale list. We accept either the hyphen or
/// snake_case form of the user-supplied name to spare them having to
/// guess which one cargo used internally.
fn promote_pruned_targets_to_stale(
    profile_dir: &Path,
    inv: &mut Inventory,
    prune_targets: &[String],
) -> Result<()> {
    let prune_set: HashSet<String> = prune_targets
        .iter()
        .flat_map(|t| [t.clone(), to_snake(t)])
        .collect();
    let fp_dir = profile_dir.join(".fingerprint");
    if !fp_dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&fp_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let (crate_name, hash) = match parse_unit_dirname(&name) {
            Some(p) => p,
            None => continue,
        };
        if prune_set.contains(crate_name) || prune_set.contains(to_snake(crate_name).as_str()) {
            // Move it from "live" to "stale" — drop the hash so the deps
            // and build sweeps cascade, and add the path to the removal list.
            if inv.live_hashes.remove(hash) {
                inv.stale_fingerprints.push(entry.path());
            } else if !inv.stale_fingerprints.iter().any(|p| p == &entry.path()) {
                // Already on the stale list (e.g. via age) — leave it there.
                inv.stale_fingerprints.push(entry.path());
            }
        }
    }
    Ok(())
}

fn sweep_deps(
    profile_dir: &Path,
    inv: &Inventory,
    opts: &SweepOptions,
    report: &mut SweepReport,
) -> Result<()> {
    let deps = profile_dir.join("deps");
    if !deps.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&deps)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if let Some(hash) = extract_deps_hash(name_str) {
            if !inv.live_hashes.contains(hash) {
                remove_file(&entry.path(), opts, report)?;
            }
        }
        // Files without an extractable hash (extremely rare in deps/)
        // are left alone — better safe than sorry.
    }
    Ok(())
}

fn sweep_build(
    profile_dir: &Path,
    inv: &Inventory,
    opts: &SweepOptions,
    report: &mut SweepReport,
) -> Result<()> {
    let build = profile_dir.join("build");
    if !build.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&build)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((_, hash)) = parse_unit_dirname(&name) {
            if !inv.live_hashes.contains(hash) {
                remove_dir(&entry.path(), opts, report)?;
            }
        }
    }
    Ok(())
}

fn sweep_incremental(
    profile_dir: &Path,
    live: &LiveSet,
    opts: &SweepOptions,
    prune_targets: &[String],
    report: &mut SweepReport,
) -> Result<()> {
    let inc = profile_dir.join("incremental");
    if !inc.is_dir() {
        return Ok(());
    }
    let prune_set: HashSet<String> = prune_targets
        .iter()
        .flat_map(|t| [t.clone(), to_snake(t)])
        .collect();
    for entry in std::fs::read_dir(&inc)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Format: <crate_snake>-<rustc_short_hash>. The rustc hash is
        // base36 of variable length, so we just split on the last '-'.
        let crate_name = match name.rfind('-') {
            Some(idx) => &name[..idx],
            None => continue,
        };
        // Explicit prune of this target overrides everything else.
        if prune_set.contains(crate_name) {
            remove_dir(&path, opts, report)?;
            continue;
        }
        if !live.contains_snake(crate_name) && crate_name != "build_script_build" {
            remove_dir(&path, opts, report)?;
            continue;
        }
        // For surviving live-crate incremental dirs, prune older rustc
        // sessions. Cargo only consults the most recent one.
        prune_old_sessions(&path, opts, report)?;
    }
    Ok(())
}

fn prune_old_sessions(
    crate_inc_dir: &Path,
    opts: &SweepOptions,
    report: &mut SweepReport,
) -> Result<()> {
    let mut sessions: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in std::fs::read_dir(crate_inc_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("s-") {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        sessions.push((entry.path(), mtime));
    }
    if sessions.len() <= opts.keep_sessions {
        return Ok(());
    }
    // Newest first
    sessions.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in sessions.into_iter().skip(opts.keep_sessions) {
        // Also clean up the matching .lock file alongside the session dir.
        let lock = path.with_extension("lock");
        remove_dir(&path, opts, report)?;
        if lock.is_file() {
            remove_file(&lock, opts, report)?;
        }
    }
    Ok(())
}

fn sweep_top_level(
    profile_dir: &Path,
    inv: &Inventory,
    live: &LiveSet,
    opts: &SweepOptions,
    report: &mut SweepReport,
) -> Result<()> {
    // Files directly in <profile>/ are workspace bins/libs and their
    // depinfo siblings: `myapp`, `myapp.d`, `libmyapp.rlib`, etc. They are
    // hardlinked into deps/ and we want to keep them iff their underlying
    // crate name is in the live set.
    let prune_set: HashSet<String> = opts
        .prune_targets
        .iter()
        .flat_map(|t| [t.clone(), to_snake(t)])
        .collect();
    for entry in std::fs::read_dir(profile_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if name_str == ".cargo-lock" {
            continue;
        }
        if file_stem_matches(name_str, &prune_set) {
            remove_file(&entry.path(), opts, report)?;
            continue;
        }
        if name_matches_live(name_str, inv, live) {
            continue;
        }
        remove_file(&entry.path(), opts, report)?;
    }
    Ok(())
}

/// Same logic as `name_matches_live` for the stem-vs-name comparison, but
/// against an arbitrary set instead of `LiveSet`. Handles `lib<name>`
/// prefix and `<name>-<16hex>` hash suffix.
fn file_stem_matches(filename: &str, names: &HashSet<String>) -> bool {
    let stem = match filename.rfind('.') {
        Some(idx) => &filename[..idx],
        None => filename,
    };
    let root = match parse_unit_dirname(stem) {
        Some((r, _)) => r,
        None => stem,
    };
    if names.contains(root) {
        return true;
    }
    if let Some(rest) = root.strip_prefix("lib") {
        if names.contains(rest) {
            return true;
        }
    }
    false
}

fn sweep_examples(
    profile_dir: &Path,
    inv: &Inventory,
    live: &LiveSet,
    opts: &SweepOptions,
    report: &mut SweepReport,
) -> Result<()> {
    let ex = profile_dir.join("examples");
    if !ex.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&ex)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        if name_matches_live(name_str, inv, live) {
            continue;
        }
        remove_file(&entry.path(), opts, report)?;
    }
    Ok(())
}

/// True if `filename` matches a known-live target. Handles:
///   - bare `<name>` and `<name>.<ext>`
///   - `lib<name>.<ext>` (workspace library outputs)
///   - `<name>-<16hex>[.ext]` and `lib<name>-<16hex>[.ext]` (hashed deps copies)
fn name_matches_live(filename: &str, inv: &Inventory, live: &LiveSet) -> bool {
    let stem = match filename.rfind('.') {
        Some(idx) => &filename[..idx],
        None => filename,
    };

    // If there's a trailing -<16hex>, the canonical "live" check is the
    // hash set; the name itself is whatever comes before.
    if let Some((root, hash)) = parse_unit_dirname(stem) {
        if inv.live_hashes.contains(hash) {
            return true;
        }
        if matches_name(root, live) {
            return true;
        }
        return false;
    }

    matches_name(stem, live)
}

fn matches_name(stem: &str, live: &LiveSet) -> bool {
    if live.matches_any(stem) {
        return true;
    }
    if let Some(rest) = stem.strip_prefix("lib") {
        if live.matches_any(rest) {
            return true;
        }
    }
    false
}

fn remove_file(path: &Path, opts: &SweepOptions, report: &mut SweepReport) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(()),
    };
    let size = meta.len();
    if opts.verbose {
        println!("  rm {} ({})", path.display(), human(size));
    }
    if !opts.dry_run {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    }
    report.removed_files += 1;
    report.freed_bytes += size;
    Ok(())
}

fn remove_dir(path: &Path, opts: &SweepOptions, report: &mut SweepReport) -> Result<()> {
    let size = dir_size(path);
    if opts.verbose {
        println!("  rm -r {} ({})", path.display(), human(size));
    }
    if !opts.dry_run {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    report.removed_dirs += 1;
    report.freed_bytes += size;
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

pub fn human(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut idx = 0;
    while v >= 1024.0 && idx < UNITS.len() - 1 {
        v /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else {
        format!("{:.1} {}", v, UNITS[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inv_with(hashes: &[&str]) -> Inventory {
        Inventory {
            live_hashes: hashes.iter().map(|s| s.to_string()).collect(),
            stale_fingerprints: Vec::new(),
        }
    }

    #[test]
    fn matches_workspace_bin_no_hash() {
        let inv = inv_with(&[]);
        let live = LiveSet::from_names(["myapp"]);
        assert!(name_matches_live("myapp", &inv, &live));
        assert!(name_matches_live("myapp.d", &inv, &live));
    }

    #[test]
    fn matches_workspace_bin_when_target_name_differs_from_package_name() {
        // This is the bug we hit with the fixture: package name is
        // `cargo-gc-fixture` but the bin target is `fixture`. The top-level
        // executable on disk is `fixture`, and it must be kept.
        let inv = inv_with(&[]);
        let live = LiveSet::from_names(["cargo-gc-fixture", "fixture"]);
        assert!(name_matches_live("fixture", &inv, &live));
        assert!(name_matches_live("fixture.d", &inv, &live));
    }

    #[test]
    fn matches_workspace_lib_with_lib_prefix() {
        let inv = inv_with(&[]);
        let live = LiveSet::from_names(["myapp"]);
        assert!(name_matches_live("libmyapp.rlib", &inv, &live));
        assert!(name_matches_live("libmyapp.d", &inv, &live));
    }

    #[test]
    fn matches_hashed_dep_via_hash() {
        let inv = inv_with(&["9b9bb928814887d2"]);
        let live = LiveSet::from_names(std::iter::empty::<&str>());
        assert!(name_matches_live(
            "libcfg_if-9b9bb928814887d2.rlib",
            &inv,
            &live
        ));
        assert!(name_matches_live("cfg_if-9b9bb928814887d2.d", &inv, &live));
    }

    #[test]
    fn rejects_unknown_top_level_file() {
        let inv = inv_with(&[]);
        let live = LiveSet::from_names(["myapp"]);
        assert!(!name_matches_live("oldname", &inv, &live));
        assert!(!name_matches_live("oldname.d", &inv, &live));
        assert!(!name_matches_live("liboldname.rlib", &inv, &live));
    }

    #[test]
    fn rejects_hashed_dep_unknown_hash() {
        let inv = inv_with(&["1234567890abcdef"]);
        let live = LiveSet::from_names(std::iter::empty::<&str>());
        assert!(!name_matches_live(
            "libfoo-deadbeefdeadbeef.rlib",
            &inv,
            &live
        ));
    }

    #[test]
    fn report_merge_accumulates() {
        let mut a = SweepReport {
            removed_files: 1,
            removed_dirs: 2,
            freed_bytes: 100,
        };
        let b = SweepReport {
            removed_files: 3,
            removed_dirs: 4,
            freed_bytes: 50,
        };
        a.merge(&b);
        assert_eq!(a.removed_files, 4);
        assert_eq!(a.removed_dirs, 6);
        assert_eq!(a.freed_bytes, 150);
    }

    #[test]
    fn human_units() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 K");
        assert_eq!(human(5 * 1024 * 1024), "5.0 M");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 G");
    }

    #[test]
    fn file_stem_matches_handles_lib_prefix_and_hash_suffix() {
        let mut names = HashSet::new();
        names.insert("model_viewer".to_string());
        // bare bin
        assert!(file_stem_matches("model_viewer", &names));
        assert!(file_stem_matches("model_viewer.d", &names));
        // workspace lib
        assert!(file_stem_matches("libmodel_viewer.rlib", &names));
        // hashed deps copy
        assert!(file_stem_matches(
            "libmodel_viewer-1234567890abcdef.rlib",
            &names
        ));
        assert!(file_stem_matches("model_viewer-1234567890abcdef.d", &names));
        // unrelated
        assert!(!file_stem_matches("otherapp", &names));
        assert!(!file_stem_matches("libserde-deadbeefdeadbeef.rlib", &names));
    }

    #[test]
    fn file_stem_matches_accepts_either_form() {
        let mut names = HashSet::new();
        names.insert("model-viewer".to_string());
        names.insert("model_viewer".to_string());
        assert!(file_stem_matches("model-viewer", &names));
        assert!(file_stem_matches("model_viewer", &names));
    }
}
