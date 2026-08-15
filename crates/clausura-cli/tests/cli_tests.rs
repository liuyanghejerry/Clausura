use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_help_output() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Run a Clausura task"))
        .stdout(predicate::str::contains("Manage checkpoints"));
}

#[test]
fn test_run_help() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--config"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--vendor"))
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn test_run_dry_run() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args(["run", "--dry-run"]).assert().failure();
}

#[test]
fn test_snapshot_help() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args(["snapshot", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("delete"));
}

#[test]
fn test_snapshot_list() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args(["snapshot", "list"]).assert().success();
}

#[test]
fn test_eval_help() {
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args(["eval", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--config"))
        .stdout(predicate::str::contains("--baseline"))
        .stdout(predicate::str::contains("--runs"));
}

#[test]
fn test_eval_missing_key_fails_loud() {
    let tmp = std::env::temp_dir().join(format!("clausura-eval-smoke-{}", std::process::id()));
    let mut cmd = Command::cargo_bin("clausura").unwrap();
    cmd.args([
        "eval",
        "--config",
        "../../eval/eval.yaml",
        "--scenario",
        "security-basics",
        "--out-dir",
        tmp.to_str().unwrap(),
    ])
    .env_remove("CLAUSURA_API_KEY")
    .assert()
    .failure()
    .stderr(predicate::str::contains("Missing API key"));
    let _ = std::fs::remove_dir_all(&tmp);
}
