//! Dual-mandate verification:
//!
//!   1. After `cargo build` + `cargo gc`, a second `cargo build` must
//!      compile zero units (no extension of build time).
//!   2. After the first `cargo gc`, a second `cargo gc` must remove zero
//!      files (idempotence — converged on a fixed point).
//!
//! These tests are slow because they actually invoke `cargo build` for the
//! `cargo-gc-fixture` workspace member into a tempdir. Marked `#[ignore]`
//! so the normal `cargo test` stays fast; run with:
//!
//!     cargo test --test no_recompile -- --include-ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_manifest() -> PathBuf {
    workspace_root().join("fixture/Cargo.toml")
}

fn cargo_gc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cargo-gc"))
}

/// Pick a parent for the test tempdir.
///
/// These tests build the fixture workspace member, which produces a few
/// hundred MB of artifacts. If your `/tmp` is on a small tmpfs, set
/// `$CGC_TEST_TMP_DIR=/path/to/big/disk` and the tempdir will land there
/// instead.
fn tempdir() -> tempfile::TempDir {
    if let Ok(p) = std::env::var("CGC_TEST_TMP_DIR") {
        std::fs::create_dir_all(&p).ok();
        return tempfile::tempdir_in(p).expect("tempdir in CGC_TEST_TMP_DIR");
    }
    tempfile::tempdir().expect("system tempdir")
}

/// Run `cargo build -p cargo-gc-fixture` against the given target dir,
/// returning how many units cargo reported "Compiling" for.
fn build_fixture(target_dir: &Path, release: bool) -> usize {
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("-p")
        .arg("cargo-gc-fixture")
        .arg("--manifest-path")
        .arg(fixture_manifest())
        .env("CARGO_TARGET_DIR", target_dir);
    if release {
        cmd.arg("--release");
    }
    let out = cmd
        .output()
        .expect("failed to spawn cargo build for fixture");
    assert!(
        out.status.success(),
        "cargo build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.trim_start().starts_with("Compiling "))
        .count()
}

/// Invoke our cargo-gc binary against the given target dir.
/// Returns (removed_files+removed_dirs total, raw summary line).
fn run_gc(target_dir: &Path, dry_run: bool) -> (usize, String) {
    run_gc_with(target_dir, dry_run, None)
}

fn run_gc_with(target_dir: &Path, dry_run: bool, max_age: Option<&str>) -> (usize, String) {
    run_gc_full(target_dir, dry_run, max_age, &[], &[])
}

fn run_gc_full(
    target_dir: &Path,
    dry_run: bool,
    max_age: Option<&str>,
    prune_profiles: &[&str],
    prune_targets: &[&str],
) -> (usize, String) {
    let mut cmd = Command::new(cargo_gc_bin());
    cmd.arg("--target-dir")
        .arg(target_dir)
        .arg("--manifest-path")
        .arg(fixture_manifest());
    if dry_run {
        cmd.arg("--dry-run");
    }
    if let Some(a) = max_age {
        cmd.arg("--max-age").arg(a);
    }
    for p in prune_profiles {
        cmd.arg("--prune-profile").arg(p);
    }
    for t in prune_targets {
        cmd.arg("--prune-target").arg(t);
    }
    let out = cmd.output().expect("failed to spawn cargo-gc");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "cargo-gc failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    // Parse the summary line: "<files> files, <dirs> dirs, freed <…>"
    let summary = stdout.lines().last().unwrap_or("").to_string();
    let parts: Vec<&str> = summary.split_whitespace().collect();
    let files: usize = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let dirs: usize = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    (files + dirs, summary)
}

#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn dual_mandate_debug_only() {
    let tmp = tempdir();
    let target = tmp.path().join("target");

    // 1. Cold build
    let n1 = build_fixture(&target, false);
    assert!(n1 > 0, "cold build should compile units");

    // 2. GC
    let (removed1, summary1) = run_gc(&target, false);
    eprintln!("first gc: {}", summary1);

    // 3. Rebuild — must compile nothing
    let n2 = build_fixture(&target, false);
    assert_eq!(
        n2, 0,
        "second build after gc must compile zero units (extends build time otherwise)"
    );

    // 4. Second GC — must remove nothing
    let (removed2, summary2) = run_gc(&target, false);
    eprintln!("second gc: {}", summary2);
    assert_eq!(
        removed2, 0,
        "second gc must remove zero items (idempotence). first removed {} items",
        removed1
    );
}

#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn dual_mandate_debug_and_release() {
    let tmp = tempdir();
    let target = tmp.path().join("target");

    let nd1 = build_fixture(&target, false);
    let nr1 = build_fixture(&target, true);
    assert!(nd1 > 0 && nr1 > 0);

    let (_removed, summary) = run_gc(&target, false);
    eprintln!("after debug+release gc: {}", summary);

    let nd2 = build_fixture(&target, false);
    let nr2 = build_fixture(&target, true);
    assert_eq!(nd2, 0, "debug rebuild must be a no-op after gc");
    assert_eq!(nr2, 0, "release rebuild must be a no-op after gc");

    let (removed2, _) = run_gc(&target, false);
    assert_eq!(removed2, 0, "second gc must be a no-op");
}

#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn cleans_orphans_but_keeps_live_files() {
    // Build the fixture, drop a fake orphan into deps/ with a hash that
    // doesn't appear in any fingerprint dir, run gc, and verify ONLY the
    // orphan was removed.
    let tmp = tempdir();
    let target = tmp.path().join("target");

    build_fixture(&target, false);

    let deps = target.join("debug/deps");
    let orphan_lib = deps.join("liborphan-ffffffffffffffff.rlib");
    let orphan_bin = deps.join("orphan-ffffffffffffffff.d");
    std::fs::write(&orphan_lib, b"fake rlib content").unwrap();
    std::fs::write(&orphan_bin, b"fake .d content").unwrap();

    let (removed, summary) = run_gc(&target, false);
    eprintln!("orphan-cleaning gc: {}", summary);

    assert!(
        !orphan_lib.exists(),
        "orphan rlib should have been swept: {}",
        orphan_lib.display()
    );
    assert!(
        !orphan_bin.exists(),
        "orphan dep-info should have been swept: {}",
        orphan_bin.display()
    );
    assert_eq!(
        removed, 2,
        "exactly the two orphans should have been removed"
    );

    // Sanity: a no-op rebuild after the cleaning gc.
    let n = build_fixture(&target, false);
    assert_eq!(n, 0, "rebuild after orphan cleanup must compile nothing");

    // And the next gc must find nothing to remove.
    let (removed2, _) = run_gc(&target, false);
    assert_eq!(removed2, 0);
}

/// `--max-age` must still satisfy the dual mandate when paired with the
/// canonical (build → gc) loop: a fresh build's fingerprints all share the
/// moment-of-build mtime, so a 1-hour age cutoff is in the future relative
/// to the build and prunes nothing.
#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn max_age_is_no_op_after_fresh_build() {
    let tmp = tempdir();
    let target = tmp.path().join("target");
    build_fixture(&target, false);

    let (removed, summary) = run_gc_with(&target, false, Some("1 hour"));
    eprintln!("--max-age '1 hour' after fresh build: {}", summary);
    assert_eq!(
        removed, 0,
        "max-age must not touch fingerprints whose build just happened"
    );

    let n = build_fixture(&target, false);
    assert_eq!(n, 0, "rebuild after max-age gc must be a no-op");

    let (removed2, _) = run_gc_with(&target, false, Some("1 hour"));
    assert_eq!(removed2, 0);
}

/// `--prune-profile release` should wipe `target/release/` entirely while
/// leaving `target/debug/` untouched.
#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn prune_profile_wipes_one_profile_only() {
    let tmp = tempdir();
    let target = tmp.path().join("target");
    build_fixture(&target, false);
    build_fixture(&target, true);

    assert!(target.join("debug").is_dir());
    assert!(target.join("release").is_dir());

    let (_removed, summary) = run_gc_full(&target, false, None, &["release"], &[]);
    eprintln!("--prune-profile release: {}", summary);

    assert!(
        !target.join("release").exists(),
        "release dir should be gone"
    );
    assert!(target.join("debug").is_dir(), "debug dir must survive");

    // Debug rebuild after the prune is still a no-op.
    let n = build_fixture(&target, false);
    assert_eq!(
        n, 0,
        "debug rebuild must be unaffected by --prune-profile release"
    );
}

/// `--prune-target fixture` should wipe the fixture binary's own artifacts
/// but leave shared dependency artifacts (serde, regex, …) intact, so a
/// rebuild of the fixture only needs to recompile/relink the bin itself.
#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn prune_target_wipes_bin_artifacts_but_leaves_deps() {
    let tmp = tempdir();
    let target = tmp.path().join("target");
    build_fixture(&target, false);

    // Sanity: the fixture bin and a couple of well-known dep artifacts exist.
    let bin = target.join("debug/fixture");
    let deps = target.join("debug/deps");
    assert!(bin.exists(), "fixture bin should exist after build");
    let serde_count_before = std::fs::read_dir(&deps)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("libserde-"))
        .count();
    assert!(
        serde_count_before > 0,
        "expected at least one serde rlib in deps/"
    );

    let (_removed, summary) = run_gc_full(&target, false, None, &[], &["fixture"]);
    eprintln!("--prune-target fixture: {}", summary);

    assert!(!bin.exists(), "fixture bin must be gone");

    let serde_count_after = std::fs::read_dir(&deps)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("libserde-"))
        .count();
    assert_eq!(
        serde_count_after, serde_count_before,
        "shared dep artifacts (serde) must NOT have been touched"
    );

    // Rebuild only needs to recompile the fixture bin itself, not its
    // transitive dep tree. We don't assert n==0 here (the bin compile
    // is required), but we DO assert the count is small relative to a
    // cold build.
    let n = build_fixture(&target, false);
    assert!(
        n < 5,
        "after pruning just the fixture target, rebuild should compile only \
         the bin and maybe its glue, not the whole world (got n={})",
        n
    );
}

#[test]
#[ignore = "slow: invokes cargo build on the fixture workspace member"]
fn idempotence_loop() {
    let tmp = tempdir();
    let target = tmp.path().join("target");

    build_fixture(&target, false);
    run_gc(&target, false);

    for i in 0..5 {
        let n = build_fixture(&target, false);
        assert_eq!(n, 0, "iteration {}: build must compile nothing", i);
        let (r, _) = run_gc(&target, false);
        assert_eq!(r, 0, "iteration {}: gc must remove nothing", i);
    }
}
