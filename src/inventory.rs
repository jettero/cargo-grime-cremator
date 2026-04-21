//! Walk a profile dir's `.fingerprint/` and build the live-hash inventory.
//!
//! Each `<profile>/.fingerprint/<crate>-<HASH>/` entry is a "unit" cargo
//! has compiled. The 16-hex-char trailing hash is the same hash that
//! appears in `<profile>/deps/<crate_snake>-<HASH>.{rlib,rmeta,d,…}` and
//! in `<profile>/build/<crate>-<HASH>/`. So if we keep the union of those
//! hashes (filtered to live crates) we know exactly what's referenced.

use anyhow::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::live_set::LiveSet;

#[derive(Debug, Default)]
pub struct Inventory {
    /// 16-hex-char hashes whose fingerprint dir corresponds to a live crate
    /// AND (if `--max-age` is set) is newer than the cutoff. Anything in
    /// `deps/`, `build/`, etc. with a hash NOT in this set is safe to remove.
    pub live_hashes: HashSet<String>,
    /// Fingerprint dirs marked for sweep — either because their crate isn't
    /// in the live set, or because the unit hasn't been touched since the
    /// `max_age` cutoff.
    pub stale_fingerprints: Vec<PathBuf>,
}

impl Inventory {
    /// Build the inventory.
    ///
    /// * `cutoff = None` — strict default: a fingerprint is stale only if
    ///   its crate is gone from `cargo metadata`.
    ///
    /// * `cutoff = Some(t)` — also mark a fingerprint stale if its
    ///   `invoked.timestamp` mtime is strictly older than `t`. This
    ///   targets orphaned per-unit configurations (e.g. an old feature-flag
    ///   build that cargo will never reuse). Pair this with a fresh
    ///   `cargo build` immediately before running cargo-gc — the build
    ///   touches every current unit's timestamp, so the only fingerprints
    ///   that *stay* old are the genuinely orphaned ones.
    pub fn build(profile_dir: &Path, live: &LiveSet, cutoff: Option<SystemTime>) -> Result<Self> {
        let mut inv = Inventory::default();
        let fp_dir = profile_dir.join(".fingerprint");
        if !fp_dir.is_dir() {
            return Ok(inv);
        }

        for entry in std::fs::read_dir(&fp_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (crate_name, hash) = match parse_unit_dirname(&name) {
                Some(parts) => parts,
                None => continue, // unparseable → leave alone
            };
            if !live.contains(crate_name) {
                inv.stale_fingerprints.push(path);
                continue;
            }
            if let Some(cutoff) = cutoff {
                // Default to "very fresh" when invoked.timestamp can't be
                // read so we don't accidentally sweep something we can't
                // measure.
                let invoked = invoked_mtime(&path).unwrap_or(SystemTime::now());
                if invoked < cutoff {
                    inv.stale_fingerprints.push(path);
                    continue;
                }
            }
            inv.live_hashes.insert(hash.to_string());
        }

        Ok(inv)
    }
}

pub(crate) fn invoked_mtime(fingerprint_dir: &Path) -> Option<SystemTime> {
    let ts = fingerprint_dir.join("invoked.timestamp");
    if let Ok(meta) = std::fs::metadata(&ts) {
        if let Ok(m) = meta.modified() {
            return Some(m);
        }
    }
    if let Ok(meta) = std::fs::metadata(fingerprint_dir) {
        if let Ok(m) = meta.modified() {
            return Some(m);
        }
    }
    None
}

/// Parse `name-deadbeefdeadbeef` into `("name", "deadbeefdeadbeef")`.
/// Returns None unless the trailing segment is exactly 16 lowercase hex chars.
pub fn parse_unit_dirname(s: &str) -> Option<(&str, &str)> {
    let dash = s.rfind('-')?;
    if dash == 0 {
        return None;
    }
    let (name, sep_hash) = s.split_at(dash);
    let hash = &sep_hash[1..];
    if is_hash16(hash) {
        Some((name, hash))
    } else {
        None
    }
}

/// Extract a 16-hex-char hash from a deps/ filename like `libfoo-<hash>.rlib`,
/// `foo-<hash>.d`, or `foo-<hash>` (executable, no extension).
pub fn extract_deps_hash(filename: &str) -> Option<&str> {
    let stem = match filename.rfind('.') {
        Some(idx) => &filename[..idx],
        None => filename,
    };
    let dash = stem.rfind('-')?;
    let hash = &stem[dash + 1..];
    if is_hash16(hash) {
        Some(hash)
    } else {
        None
    }
}

pub fn to_snake(s: &str) -> String {
    s.replace('-', "_")
}

fn is_hash16(s: &str) -> bool {
    s.len() == 16
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_unit_name() {
        let (n, h) = parse_unit_dirname("serde-fbbf7037c381b247").unwrap();
        assert_eq!(n, "serde");
        assert_eq!(h, "fbbf7037c381b247");
    }

    #[test]
    fn parses_hyphenated_crate_name() {
        let (n, h) = parse_unit_dirname("cfg-if-9b9bb928814887d2").unwrap();
        assert_eq!(n, "cfg-if");
        assert_eq!(h, "9b9bb928814887d2");
    }

    #[test]
    fn parses_double_hyphenated_crate_name() {
        let (n, h) = parse_unit_dirname("crossbeam-utils-81f5350a630d1f64").unwrap();
        assert_eq!(n, "crossbeam-utils");
        assert_eq!(h, "81f5350a630d1f64");
    }

    #[test]
    fn rejects_short_hash() {
        assert!(parse_unit_dirname("foo-deadbeef").is_none());
    }

    #[test]
    fn rejects_uppercase_hex() {
        assert!(parse_unit_dirname("foo-DEADBEEFDEADBEEF").is_none());
    }

    #[test]
    fn rejects_non_hex() {
        assert!(parse_unit_dirname("foo-zzzzzzzzzzzzzzzz").is_none());
    }

    #[test]
    fn rejects_no_hash_segment() {
        assert!(parse_unit_dirname("invoked.timestamp").is_none());
        assert!(parse_unit_dirname("just-a-name").is_none());
    }

    #[test]
    fn extracts_hash_from_rlib() {
        assert_eq!(
            extract_deps_hash("libcfg_if-9b9bb928814887d2.rlib"),
            Some("9b9bb928814887d2")
        );
    }

    #[test]
    fn extracts_hash_from_rmeta() {
        assert_eq!(
            extract_deps_hash("liblog-7752fdb1f5381521.rmeta"),
            Some("7752fdb1f5381521")
        );
    }

    #[test]
    fn extracts_hash_from_dep_info() {
        assert_eq!(
            extract_deps_hash("cfg_if-9b9bb928814887d2.d"),
            Some("9b9bb928814887d2")
        );
    }

    #[test]
    fn extracts_hash_from_executable() {
        assert_eq!(
            extract_deps_hash("myapp-1234567890abcdef"),
            Some("1234567890abcdef")
        );
    }

    #[test]
    fn no_hash_in_uninteresting_file() {
        assert_eq!(extract_deps_hash(".cargo-lock"), None);
        assert_eq!(extract_deps_hash("output"), None);
    }

    #[test]
    fn snake_case_conversion() {
        assert_eq!(to_snake("cfg-if"), "cfg_if");
        assert_eq!(to_snake("crossbeam-utils"), "crossbeam_utils");
        assert_eq!(to_snake("serde"), "serde");
    }

    use crate::live_set::LiveSet;
    use std::fs;
    use std::time::{Duration, SystemTime};
    use tempfile::tempdir;

    fn make_fp_dir(profile: &Path, name: &str, age_secs: u64) -> std::path::PathBuf {
        let d = profile.join(".fingerprint").join(name);
        fs::create_dir_all(&d).unwrap();
        let ts = d.join("invoked.timestamp");
        fs::write(&ts, b"").unwrap();
        let now = SystemTime::now();
        let new_time = now - Duration::from_secs(age_secs);
        let f = std::fs::OpenOptions::new().write(true).open(&ts).unwrap();
        f.set_modified(new_time).unwrap();
        d
    }

    fn cutoff_hours_ago(hours: u64) -> SystemTime {
        SystemTime::now() - Duration::from_secs(hours * 3600)
    }

    #[test]
    fn strict_mode_keeps_all_live_crate_fingerprints() {
        let dir = tempdir().unwrap();
        let profile = dir.path().to_path_buf();
        // Both fingerprints belong to live crate "foo" — keep both no matter
        // how old they are, because age-based pruning is off.
        make_fp_dir(&profile, "foo-1111111111111111", 0);
        make_fp_dir(&profile, "foo-2222222222222222", 86_400 * 30);
        // This one belongs to a removed crate.
        make_fp_dir(&profile, "gone-3333333333333333", 0);
        let live = LiveSet::from_names(["foo"]);
        let inv = Inventory::build(&profile, &live, None).unwrap();
        assert_eq!(inv.live_hashes.len(), 2);
        assert_eq!(inv.stale_fingerprints.len(), 1);
        assert!(inv.stale_fingerprints[0].to_string_lossy().contains("gone"));
    }

    #[test]
    fn max_age_prunes_units_older_than_cutoff() {
        let dir = tempdir().unwrap();
        let profile = dir.path().to_path_buf();
        make_fp_dir(&profile, "foo-1111111111111111", 0); // touched now
        make_fp_dir(&profile, "foo-2222222222222222", 60 * 30); // 30 minutes old
        make_fp_dir(&profile, "foo-3333333333333333", 86_400 * 30); // 30 days old
        let live = LiveSet::from_names(["foo"]);
        // Cutoff = 1 hour ago. Anything older than 1 hour is stale.
        let inv = Inventory::build(&profile, &live, Some(cutoff_hours_ago(1))).unwrap();
        assert_eq!(inv.live_hashes.len(), 2, "the two fresh units survive");
        assert_eq!(inv.stale_fingerprints.len(), 1);
        assert!(
            inv.stale_fingerprints[0]
                .to_string_lossy()
                .contains("3333333333333333"),
            "expected the 30-day-old dir to be marked stale"
        );
    }

    #[test]
    fn max_age_keeps_recent_singleton_crate() {
        let dir = tempdir().unwrap();
        let profile = dir.path().to_path_buf();
        make_fp_dir(&profile, "foo-1111111111111111", 0);
        let live = LiveSet::from_names(["foo"]);
        let inv = Inventory::build(&profile, &live, Some(cutoff_hours_ago(1))).unwrap();
        assert_eq!(inv.live_hashes.len(), 1);
        assert!(inv.stale_fingerprints.is_empty());
    }

    #[test]
    fn max_age_nukes_unbuilt_singleton_after_cutoff() {
        let dir = tempdir().unwrap();
        let profile = dir.path().to_path_buf();
        // Single unit, aged 1 year. With age-pruning enabled and a
        // 1-hour-ago cutoff, it goes — there's no "newer twin" to anchor
        // against. This is the deliberate "didn't build before running gc"
        // risk.
        make_fp_dir(&profile, "foo-1111111111111111", 86_400 * 365);
        let live = LiveSet::from_names(["foo"]);
        let inv = Inventory::build(&profile, &live, Some(cutoff_hours_ago(1))).unwrap();
        assert!(inv.live_hashes.is_empty());
        assert_eq!(inv.stale_fingerprints.len(), 1);
    }
}
