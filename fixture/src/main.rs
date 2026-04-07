//! Fixture binary used by cargo-grime-cremator's integration tests.
//!
//! Pulls in a handful of crates that exercise the same on-disk patterns as
//! a large bevy/rapier app (proc-macros, build scripts, hyphenated crate
//! names, transitive deps) without the multi-hour compile time.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Greeting {
    target: String,
    count: usize,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();

    let g = Greeting {
        target: "world".to_string(),
        count: 1,
    };
    let json = serde_json::to_string(&g).unwrap();
    log::info!("greeting json: {}", json);

    let re = regex::Regex::new(r"^[a-z]+$").unwrap();
    let matches = ["hello", "Hello", "world"]
        .iter()
        .filter(|s| re.is_match(s))
        .count();
    log::info!("regex matches: {}", matches);

    use rayon::prelude::*;
    let sum: u64 = (1u64..=10).into_par_iter().sum();
    log::info!("rayon sum: {}", sum);

    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    println!("fixture ok ({} {} {})", g.target, matches, sum);
}
