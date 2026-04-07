//! Command-line interface for `cargo-gc`.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-gc",
    bin_name = "cargo gc",
    version,
    about = "Garbage-collect stale files from a cargo target directory",
    long_about = "Removes deps/, build/, and incremental/ artifacts that are no longer \
                  referenced by the project's current Cargo.lock — without invoking the \
                  compiler. Safe to run repeatedly: a fresh build followed by `cargo gc` \
                  is a fixed point."
)]
pub struct Cli {
    /// Show what would be removed without removing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Override the target directory (default: auto-detect from cwd / config / env).
    #[arg(long, value_name = "PATH")]
    pub target_dir: Option<PathBuf>,

    /// Override the manifest path (default: walk up from cwd to find Cargo.toml).
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Only sweep a single profile (e.g. `debug` or `release`).
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Number of recent rustc incremental sessions to keep per crate.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub keep_sessions: usize,

    /// Drop fingerprint dirs whose `invoked.timestamp` is older than this.
    /// Accepts journalctl-style strings:
    ///
    /// • Relative durations: `"1 month"`, `"4 days ago"`,
    ///   `"2 hours 30 minutes"`, `"36h"`, `"1d12h"`.
    ///
    /// • Absolute UTC timestamps: `"2025-02-21"`,
    ///   `"2025-02-21 13:00:00"`, `"2025-02-21T13:00:00"`.
    ///
    /// • The literal `"never"`, `"off"`, or `"0"` disables the age check.
    ///
    /// This targets orphaned per-unit configurations (old feature-flag
    /// combos, removed bin targets, etc.) that cargo will never reach
    /// again. Pair with a fresh `cargo build` immediately before running
    /// cargo-gc — the build touches every current unit's timestamp, so the
    /// only fingerprints that *stay* old are the genuinely orphaned ones.
    #[arg(long, value_name = "AGE", default_value = "1 month")]
    pub max_age: String,

    /// Wipe an entire profile dir (`target/<NAME>/`) regardless of age.
    /// Repeatable. NAME isn't validated against the project's known
    /// profiles — pass anything you want gone. Common cases:
    /// `--prune-profile release`, `--prune-profile test`.
    #[arg(long, value_name = "NAME", action = clap::ArgAction::Append)]
    pub prune_profile: Vec<String>,

    /// Wipe all artifacts belonging to a workspace bin/lib/example target,
    /// across every profile dir. Repeatable. Accepts either the original
    /// (hyphenated) or the snake_case form. Common cases:
    /// `--prune-target model-viewer`, `--prune-target some_old_example`.
    #[arg(long, value_name = "NAME", action = clap::ArgAction::Append)]
    pub prune_target: Vec<String>,

    /// Print every removal as it happens.
    #[arg(short, long)]
    pub verbose: bool,

    /// List workspace targets that `--prune-target` can take, then exit
    /// without sweeping. Reads from `cargo metadata`.
    #[arg(long)]
    pub list_targets: bool,

    /// List profile dirs that `--prune-profile` can take, then exit
    /// without sweeping. Reads from disk (only profiles that have actually
    /// been built show up).
    #[arg(long)]
    pub list_profiles: bool,

    /// Convenience: same as `--list-targets --list-profiles`.
    #[arg(long)]
    pub list: bool,
}

impl Cli {
    /// Parse arguments, transparently stripping the leading `gc` token when
    /// invoked as `cargo gc …` (cargo passes the subcommand name as argv[1]).
    pub fn parse_args() -> Self {
        let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
        if args.len() > 1 && args[1] == "gc" {
            args.remove(1);
        }
        Cli::parse_from(args)
    }

    /// True if any of the `--list*` flags are set; main should print
    /// listings and exit before any sweep happens.
    pub fn any_list(&self) -> bool {
        self.list || self.list_targets || self.list_profiles
    }

    pub fn want_list_targets(&self) -> bool {
        self.list || self.list_targets
    }

    pub fn want_list_profiles(&self) -> bool {
        self.list || self.list_profiles
    }
}
