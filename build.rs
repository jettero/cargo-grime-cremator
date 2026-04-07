use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");

    let output = Command::new("git")
        .args(["describe", "--dirty", "--tags", "--match", "v[0-9]*"])
        .output();

    let version = match output {
        Ok(output) if output.status.success() => {
            let raw = String::from_utf8_lossy(&output.stdout);
            let trimmed = raw.trim();
            // v1.0.0-5-g6d8283d -> 1.0.0-5-6d8283d
            trimmed
                .strip_prefix('v')
                .unwrap_or(trimmed)
                .replace("-g", "-")
        }
        _ => env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string()),
    };

    println!("cargo:rustc-env=APP_VERSION={version}");
}
