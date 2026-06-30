//! Differential test entrypoints — one `#[test]` per suite the bash runner
//! recognises.
//!
//! Each test shells out to `tests/scripts/run-differential-tests.sh <suite>`,
//! which orchestrates the Spark reference server and the Thunderduck release
//! binary, then runs the matching pytest selection. All tests are `#[ignore]`
//! so a plain `cargo test` does not fire them. Invoke explicitly:
//!
//! ```bash
//! # one-time:
//! ./tests/scripts/setup-differential-testing.sh
//! cargo build --release
//!
//! # per-suite (use cargo's test-name filter to pick):
//! cargo test -p thunderduck-connect-server --test differential tpch -- --ignored --nocapture
//!
//! # full sweep:
//! cargo test -p thunderduck-connect-server --test differential all  -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = .../crates/connect-server → workspace root is two levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

fn run_suite(suite: &str) {
    let root = workspace_root();
    let script = root.join("tests/scripts/run-differential-tests.sh");
    assert!(
        script.exists(),
        "{} missing — run tests/scripts/setup-differential-testing.sh first",
        script.display()
    );
    let binary = root.join("target/release/thunderduck-connect-server");
    assert!(
        binary.exists(),
        "{} missing — run `cargo build --release` first",
        binary.display()
    );

    let status = Command::new(&script)
        .arg(suite)
        .current_dir(&root)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", script.display()));

    assert!(
        status.success(),
        "differential suite '{suite}' failed (status: {status})"
    );
}

#[test]
#[ignore]
fn tpch() {
    run_suite("tpch");
}

#[test]
#[ignore]
fn tpcds() {
    run_suite("tpcds");
}

#[test]
#[ignore]
fn functions() {
    run_suite("functions");
}

#[test]
#[ignore]
fn aggregations() {
    run_suite("aggregations");
}

#[test]
#[ignore]
fn window() {
    run_suite("window");
}

#[test]
#[ignore]
fn joins() {
    run_suite("joins");
}

#[test]
#[ignore]
fn types() {
    run_suite("types");
}

#[test]
#[ignore]
fn schema() {
    run_suite("schema");
}

#[test]
#[ignore]
fn dataframe() {
    run_suite("dataframe");
}

#[test]
#[ignore]
fn datetime() {
    run_suite("datetime");
}

#[test]
#[ignore]
fn conditional() {
    run_suite("conditional");
}

#[test]
#[ignore]
fn all() {
    run_suite("all");
}
