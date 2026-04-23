use anyhow::Result;
use cargo_grime_cremator::{
    age,
    cli::Cli,
    live_set::{workspace_targets, LiveSet},
    sweep::{human, sweep_target_dir, SweepOptions},
    target_dir,
};

fn main() -> Result<()> {
    let cli = Cli::parse_args();

    // --project <dir> is shorthand for --manifest-path <dir>/Cargo.toml
    let manifest_override = cli
        .manifest_path
        .as_deref()
        .or(cli.project.as_deref())
        .map(|p| {
            if p.join("Cargo.toml").is_file() {
                p.join("Cargo.toml")
            } else {
                p.to_path_buf()
            }
        });
    let paths = target_dir::resolve(cli.target_dir.as_deref(), manifest_override.as_deref())?;

    if cli.verbose {
        println!("manifest: {}", paths.manifest_path.display());
        println!("target:   {}", paths.target_dir.display());
    }

    if cli.any_list() {
        return run_list(&cli, &paths);
    }

    let live = LiveSet::from_manifest(&paths.manifest_path)?;
    if cli.verbose {
        println!("live crates: {}", live.len());
    }

    let max_age_cutoff = if age::is_disable_sentinel(&cli.max_age) {
        None
    } else {
        Some(age::parse_cutoff(&cli.max_age)?)
    };
    if cli.verbose {
        if let Some(t) = max_age_cutoff {
            println!("max-age cutoff: {}", format_systemtime(t));
        } else {
            println!("max-age check: disabled");
        }
    }

    let opts = SweepOptions {
        dry_run: cli.dry_run,
        keep_sessions: cli.keep_sessions,
        verbose: cli.verbose,
        only_profile: cli.profile,
        max_age_cutoff,
        prune_profiles: cli.prune_profile,
        prune_targets: cli.prune_target,
    };

    let report = sweep_target_dir(&paths.target_dir, &live, &opts)?;

    let action = if cli.dry_run { "would free" } else { "freed" };
    println!(
        "{} files, {} dirs, {} {}",
        report.removed_files,
        report.removed_dirs,
        action,
        human(report.freed_bytes)
    );

    Ok(())
}

/// Format a `SystemTime` for the verbose-mode summary line. Reports the
/// epoch second alongside an approximate "Nh ago" so the user doesn't have
/// to mentally convert.
fn format_systemtime(t: std::time::SystemTime) -> String {
    let now = std::time::SystemTime::now();
    match now.duration_since(t) {
        Ok(d) => {
            let hours = d.as_secs() / 3600;
            let secs_since_epoch = t
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("epoch {} (~{} h ago)", secs_since_epoch, hours)
        }
        Err(_) => "future".to_string(),
    }
}

fn run_list(cli: &Cli, paths: &target_dir::ProjectPaths) -> Result<()> {
    if cli.want_list_targets() {
        let mut targets = workspace_targets(&paths.manifest_path)?;
        targets.dedup_by(|a, b| a.name == b.name && a.kind == b.kind && a.package == b.package);
        println!("workspace targets ({}):", targets.len());
        for t in &targets {
            println!("  {:<8} {}  ({})", t.kind, t.name, t.package);
        }
    }
    if cli.want_list_profiles() {
        let profiles = target_dir::list_profile_dirs(&paths.target_dir)?;
        println!("on-disk profiles ({}):", profiles.len());
        for p in &profiles {
            println!("  {}", p);
        }
    }
    Ok(())
}
