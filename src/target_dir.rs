//! Resolve the cargo target directory and the manifest path for the project
//! the user is currently sitting in.
//!
//! Resolution order for target dir:
//!   1. explicit `--target-dir` flag
//!   2. `$CARGO_TARGET_DIR` env var
//!   3. `[build] target-dir = "..."` in the nearest `.cargo/config.toml`
//!      (walking up from the manifest dir)
//!   4. `<manifest_dir>/target`

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub manifest_path: PathBuf,
    pub target_dir: PathBuf,
}

pub fn resolve(
    explicit_target: Option<&Path>,
    explicit_manifest: Option<&Path>,
) -> Result<ProjectPaths> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let manifest_path = match explicit_manifest {
        Some(p) => {
            if !p.is_file() {
                return Err(anyhow!("--manifest-path {} not found", p.display()));
            }
            p.to_path_buf()
        }
        None => find_manifest(&cwd)
            .ok_or_else(|| anyhow!("no Cargo.toml found in {} or any parent", cwd.display()))?,
    };

    let target_dir = if let Some(t) = explicit_target {
        t.to_path_buf()
    } else if let Some(t) = std::env::var_os("CARGO_TARGET_DIR") {
        PathBuf::from(t)
    } else if let Some(t) = read_config_target_dir(&manifest_path)? {
        t
    } else {
        manifest_path
            .parent()
            .ok_or_else(|| anyhow!("manifest path has no parent"))?
            .join("target")
    };

    Ok(ProjectPaths {
        manifest_path,
        target_dir,
    })
}

fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

/// Find profile dir entries inside `target_dir` — direct children that
/// look like cargo profile dirs (have a `.fingerprint/` subdir). Used by
/// `--list-profiles`.
pub fn list_profile_dirs(target_dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if !target_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(target_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "doc" || name == "package" || name == "tmp" {
            continue;
        }
        if entry.path().join(".fingerprint").is_dir() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

fn read_config_target_dir(manifest_path: &Path) -> Result<Option<PathBuf>> {
    let mut current: Option<&Path> = manifest_path.parent();
    while let Some(dir) = current {
        for name in [".cargo/config.toml", ".cargo/config"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                let contents = std::fs::read_to_string(&candidate)
                    .with_context(|| format!("reading {}", candidate.display()))?;
                let parsed: toml::Value = toml::from_str(&contents)
                    .with_context(|| format!("parsing {}", candidate.display()))?;
                if let Some(target_dir) = parsed
                    .get("build")
                    .and_then(|b| b.get("target-dir"))
                    .and_then(|v| v.as_str())
                {
                    return Ok(Some(PathBuf::from(target_dir)));
                }
            }
        }
        current = dir.parent();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finds_cargo_toml_in_cwd() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let found = find_manifest(dir.path()).unwrap();
        assert_eq!(found, dir.path().join("Cargo.toml"));
    }

    #[test]
    fn walks_up_to_find_cargo_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let nested = dir.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        let found = find_manifest(&nested).unwrap();
        assert_eq!(found, dir.path().join("Cargo.toml"));
    }

    #[test]
    fn reads_config_toml_target_dir() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"x\"\n").unwrap();
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(
            dir.path().join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"/tmp/somewhere\"\n",
        )
        .unwrap();
        let target = read_config_target_dir(&manifest).unwrap();
        assert_eq!(target, Some(PathBuf::from("/tmp/somewhere")));
    }

    #[test]
    fn no_config_returns_none() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"x\"\n").unwrap();
        let target = read_config_target_dir(&manifest).unwrap();
        assert_eq!(target, None);
    }
}
