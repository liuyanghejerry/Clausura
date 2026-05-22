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
