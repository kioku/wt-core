use assert_cmd::Command;
use predicates::prelude::*;

fn wt_core() -> Command {
    Command::new(assert_cmd::cargo_bin!("wt-core"))
}

#[test]
fn init_bash_emits_binding() {
    wt_core()
        .args(["init", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wt()"));
}

#[test]
fn init_zsh_emits_binding() {
    wt_core()
        .args(["init", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("wt()"));
}

#[test]
fn init_fish_emits_binding() {
    wt_core()
        .args(["init", "fish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("function wt"));
}

#[test]
fn init_nu_emits_binding() {
    wt_core()
        .args(["init", "nu"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export def"));
}

#[test]
fn init_unknown_shell_fails() {
    wt_core()
        .args(["init", "powershell"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown shell 'powershell'"))
        .stderr(predicate::str::contains("bash, zsh, fish, nu"));
}
