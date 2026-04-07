//! cargo-grime-cremator: garbage collect stale files from cargo target
//! directories without invoking the compiler. See `README.md` and the
//! `cargo-gc` binary for end-user docs.

pub mod age;
pub mod cli;
pub mod inventory;
pub mod live_set;
pub mod sweep;
pub mod target_dir;
