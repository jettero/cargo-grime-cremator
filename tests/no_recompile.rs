//! Dual-mandate verification:
//!
//!   1. After `cargo build` + `cargo gc`, a second `cargo build` must
//!      compile zero units (no extension of build time).
//!   2. After the first `cargo gc`, a second `cargo gc` must remove zero
//!      files (idempotence — converged on a fixed point).
//!
//! These tests are slow because they actually invoke `cargo build` for the
//! `cargo-gc-fixture` workspace member into a tempdir.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

// Serialize nested `cargo build` invocations across parallel test threads.
// Without this, N parallel tests × N rustc workers each = process explosion
// that survives 128 GB locally but kills small CI runners.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

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
    let _guard = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// The zombie workspace rlib scenario (the actual ThinLTO breakage):
///
/// A workspace member was compiled against a dependency rlib. Later that dep
/// was recompiled (new metadata hash → new rlib). The workspace member's rlib
/// still exists with its old fingerprint, but its .o files reference ThinLTO
/// partition symbols from the OLD dep compilation. Linking fails.
///
/// Unit-level dedup: when a dependency has two fingerprints for the same
/// compilation unit (same target + profile in the fingerprint JSON), gc
/// should remove the older one, purge workspace member zombies, and leave
/// a state where rebuild + second gc converges.
#[test]
fn dedup_stale_dep_fingerprint_same_unit() {
    let tmp = tempdir();
    let target = tmp.path().join("target");
    build_fixture(&target, false);

    let fp_dir = target.join("debug/.fingerprint");
    let deps_dir = target.join("debug/deps");

    let real_serde_hash = find_rlib_hash(&deps_dir, "serde");
    let real_fp_dir = fp_dir.join(format!("serde-{}", real_serde_hash));
    assert!(real_fp_dir.is_dir());
    let json_file = find_json_in_fingerprint(&real_fp_dir);
    let json_content = std::fs::read_to_string(&json_file).unwrap();
    let json_name = json_file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // Plant a fake OLDER serde fingerprint (same unit type).
    let fake_hash = "ffffffffffffffff";
    let fake_fp_dir = fp_dir.join(format!("serde-{}", fake_hash));
    std::fs::create_dir_all(&fake_fp_dir).unwrap();
    std::fs::write(fake_fp_dir.join(&json_name), &json_content).unwrap();
    let ts = fake_fp_dir.join("invoked.timestamp");
    std::fs::write(&ts, b"").unwrap();
    let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&ts)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    std::fs::write(
        fake_fp_dir.join(json_name.trim_end_matches(".json")),
        b"deadbeefdeadbeef",
    )
    .unwrap();
    let fake_rlib = deps_dir.join(format!("libserde-{}.rlib", fake_hash));
    std::fs::write(&fake_rlib, b"fake stale rlib content").unwrap();

    let (_removed, summary) = run_gc(&target, false);
    eprintln!("stale-dep dedup gc: {}", summary);

    assert!(!fake_rlib.exists(), "fake stale rlib should be swept");
    assert!(
        !fake_fp_dir.exists(),
        "fake stale fingerprint should be swept"
    );
    // Real serde rlib is ALSO gone — dep with duplicates gets fully nuked
    // to avoid ThinLTO partition mismatches.
    assert!(
        !deps_dir
            .join(format!("libserde-{}.rlib", real_serde_hash))
            .exists(),
        "real serde rlib should also be removed (dep chain eviction)"
    );

    // Rebuild recompiles serde (dep was nuked) + workspace member (zombie purge).
    // Should still be much less than a full cold build.
    let cold_build_units = 50; // rough lower bound for fixture cold build
    let n = build_fixture(&target, false);
    assert!(
        n > 0 && n < cold_build_units,
        "rebuild should recompile affected deps + workspace member, \
         not everything (got {})",
        n
    );

    // build → gc → build → gc must converge. The dep chain eviction may
    // leave one orphaned incremental session on the first cycle.
    let (r2, _) = run_gc(&target, false);
    assert!(r2 <= 1, "second gc should converge (removed {})", r2);
    let n2 = build_fixture(&target, false);
    assert_eq!(n2, 0, "third build must compile nothing");
}

/// Full zombie dep chain eviction: when a dep is deduped and workspace
/// members are zombie-purged, THREE things must be removed:
///
/// 1. Zombie workspace rlib + fingerprint
/// 2. ALL rlibs for the deduped dep (not just the unit-key group)
/// 3. Workspace member incremental cache (incremental/<ws_member>-*)
///
/// Without #3, cargo does an incremental recompile reusing poisoned codegen
/// units from the cache → produces an identical broken rlib. Without #2,
/// cargo links the recompiled workspace member against the same poisoned dep.
/// All three must go together.
#[test]
fn zombie_full_chain_eviction() {
    let tmp = tempdir();
    let target = tmp.path().join("target");

    // 1. Build fixture, verify clean state.
    build_fixture(&target, false);
    let n_warmup = build_fixture(&target, false);
    assert_eq!(n_warmup, 0, "warm build should be a no-op");

    let fp_dir = target.join("debug/.fingerprint");
    let deps_dir = target.join("debug/deps");
    let inc_dir = target.join("debug/incremental");

    // 2. Note current state.
    let real_serde_hash = find_rlib_hash(&deps_dir, "serde");
    let fixture_fp_hash = find_fingerprint_hash(&fp_dir, "cargo-gc-fixture");

    // 3. Verify incremental cache exists for the fixture binary.
    //    The binary target is "fixture" so incremental dirs are "fixture-*".
    let fixture_inc_before = count_dirs_with_prefix(&inc_dir, "fixture-");
    assert!(
        fixture_inc_before > 0,
        "fixture should have incremental cache dirs before gc"
    );

    // 4. Plant a FAKE newer serde fingerprint to trigger dep dedup → zombie.
    let real_serde_fp = fp_dir.join(format!("serde-{}", real_serde_hash));
    let json_file = find_json_in_fingerprint(&real_serde_fp);
    let json_content = std::fs::read_to_string(&json_file).unwrap();
    let json_name = json_file
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let fake_hash = "ffffffffffffffff";
    let fake_fp_dir = fp_dir.join(format!("serde-{}", fake_hash));
    std::fs::create_dir_all(&fake_fp_dir).unwrap();
    std::fs::write(fake_fp_dir.join(&json_name), &json_content).unwrap();
    let ts = fake_fp_dir.join("invoked.timestamp");
    std::fs::write(&ts, b"").unwrap();
    let future_time = std::time::SystemTime::now() + std::time::Duration::from_secs(1);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&ts)
        .unwrap()
        .set_modified(future_time)
        .unwrap();
    let hash_file_name = json_name.trim_end_matches(".json");
    std::fs::write(fake_fp_dir.join(hash_file_name), b"fakeeeeeeeeeeeee").unwrap();
    std::fs::write(
        deps_dir.join(format!("libserde-{}.rlib", fake_hash)),
        b"fake newer rlib",
    )
    .unwrap();

    // 5. Run gc.
    let (_removed, summary) = run_gc(&target, false);
    eprintln!("zombie chain gc: {}", summary);

    // 6. Assert #1: zombie workspace fingerprint removed.
    let fixture_fp = fp_dir.join(format!("cargo-gc-fixture-{}", fixture_fp_hash));
    assert!(
        !fixture_fp.exists(),
        "fixture fingerprint should be removed (zombie)"
    );

    // 7. Assert #2: ALL serde rlibs removed (both real and fake).
    let serde_rlibs_after = count_files_with_prefix_suffix(&deps_dir, "libserde-", ".rlib");
    assert_eq!(
        serde_rlibs_after, 0,
        "ALL serde rlibs must be removed when serde was deduped — \
         keeping any risks recompilation against poisoned ThinLTO symbols"
    );

    // 8. Assert #3: workspace incremental cache removed.
    let fixture_inc_after = count_dirs_with_prefix(&inc_dir, "fixture-");
    assert_eq!(
        fixture_inc_after, 0,
        "fixture incremental cache must be removed when zombie is detected — \
         without this, cargo does incremental recompile using poisoned codegen \
         units and produces the same broken rlib"
    );

    // 9. Rebuild must succeed and recover.
    let n = build_fixture(&target, false);
    assert!(n > 0, "should recompile after gc cleanup");

    // 10. Second build is stable (convergence).
    let n2 = build_fixture(&target, false);
    assert_eq!(n2, 0, "recovered build should be stable");
}

/// Count directories in `dir` whose name starts with `prefix`.
fn count_dirs_with_prefix(dir: &Path, prefix: &str) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && e.file_name().to_string_lossy().starts_with(prefix)
        })
        .count()
}

/// Count files in `dir` matching prefix and suffix.
fn count_files_with_prefix_suffix(dir: &Path, prefix: &str, suffix: &str) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(prefix) && n.ends_with(suffix)
        })
        .count()
}

/// Find a fingerprint hash for a crate that has a fingerprint dir.
fn find_fingerprint_hash(fp_dir: &Path, crate_name: &str) -> String {
    let prefix = format!("{}-", crate_name);
    for entry in std::fs::read_dir(fp_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&prefix) {
            if rest.len() == 16
                && rest
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            {
                return rest.to_string();
            }
        }
    }
    panic!("no fingerprint found for crate '{}'", crate_name);
}

// ── helpers for the stale-dep test ──

/// Find the 16-hex hash of the rlib for a given crate in deps/.
/// Panics if zero or more than one rlib matches.
fn find_rlib_hash(deps_dir: &Path, crate_name: &str) -> String {
    let prefix = format!("lib{}-", crate_name.replace('-', "_"));
    let suffix = ".rlib";
    let matches: Vec<String> = std::fs::read_dir(deps_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&prefix) && n.ends_with(suffix) {
                let hash = &n[prefix.len()..n.len() - suffix.len()];
                if hash.len() == 16 {
                    return Some(hash.to_string());
                }
            }
            None
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly 1 {} rlib, found {}: {:?}",
        crate_name,
        matches.len(),
        matches
    );
    matches.into_iter().next().unwrap()
}

/// Find the .json file inside a fingerprint dir.
fn find_json_in_fingerprint(fp_dir: &Path) -> PathBuf {
    for entry in std::fs::read_dir(fp_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") {
            return entry.path();
        }
    }
    panic!("no .json file in fingerprint dir: {}", fp_dir.display());
}
