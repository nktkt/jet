use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn jet() -> Command {
    Command::cargo_bin("jet").unwrap()
}

#[test]
fn new_creates_full_project_tree() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("hello");

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created jet project `hello`"));

    assert!(project.join("jet.toml").is_file());
    assert!(project.join(".gitignore").is_file());
    assert!(project.join("src/main/java/com/example/hello/Main.java").is_file());
    assert!(project.join("src/main/resources/.gitkeep").is_file());
    assert!(project.join("src/test/java/com/example/hello").is_dir());

    let manifest = fs::read_to_string(project.join("jet.toml")).unwrap();
    assert!(manifest.contains("name    = \"hello\""));
    assert!(manifest.contains("java    = 21"));

    let main_java =
        fs::read_to_string(project.join("src/main/java/com/example/hello/Main.java")).unwrap();
    assert!(main_java.contains("package com.example.hello;"));
    assert!(main_java.contains("Hello from jet!"));

    assert!(!project.join(".git").exists(), "--no-vcs should skip git init");
}

#[test]
fn new_runs_git_init_by_default() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("vcs-app");

    jet()
        .args(["new", project.to_str().unwrap()])
        .assert()
        .success();

    assert!(
        project.join(".git").is_dir(),
        "default `jet new` should create .git/"
    );
}

#[test]
fn new_converts_hyphenated_name_to_java_package() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("my-app");

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .success();

    assert!(
        project
            .join("src/main/java/com/example/my_app/Main.java")
            .is_file(),
        "hyphens in name should become underscores in Java package"
    );
    let main_java = fs::read_to_string(
        project.join("src/main/java/com/example/my_app/Main.java"),
    )
    .unwrap();
    assert!(main_java.contains("package com.example.my_app;"));
}

#[test]
fn new_refuses_when_target_dir_is_non_empty() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("crowded");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("README.md"), "hi").unwrap();

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists and is not empty"));
}

#[test]
fn new_succeeds_when_target_dir_is_empty() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("empty-dir");
    fs::create_dir_all(&project).unwrap();

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .success();
    assert!(project.join("jet.toml").is_file());
}

#[test]
fn new_refuses_when_target_is_a_file() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("not-a-dir");
    fs::write(&path, "").unwrap();

    jet()
        .args(["new", path.to_str().unwrap(), "--no-vcs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is a file"));
}

#[test]
fn new_rejects_invalid_name() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("class");

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Java reserved word"));
}

#[test]
fn new_rejects_name_starting_with_digit() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("123app");

    jet()
        .args(["new", project.to_str().unwrap(), "--no-vcs"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot start with a digit"));
}

#[test]
fn init_scaffolds_in_existing_dir() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("preset");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("README.md"), "existing readme\n").unwrap();

    jet()
        .args(["init", "--no-vcs"])
        .current_dir(&project)
        .assert()
        .success();

    assert!(project.join("jet.toml").is_file());
    assert!(project.join("src/main/java/com/example/preset/Main.java").is_file());
    assert_eq!(
        fs::read_to_string(project.join("README.md")).unwrap(),
        "existing readme\n",
        "init must not overwrite user files"
    );
}

#[test]
fn init_preserves_existing_gitignore() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("custom-gi");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join(".gitignore"), "# my custom rules\nfoo/\n").unwrap();

    jet()
        .args(["init", "--no-vcs"])
        .current_dir(&project)
        .assert()
        .success()
        .stderr(predicate::str::contains("skipping existing"));

    assert_eq!(
        fs::read_to_string(project.join(".gitignore")).unwrap(),
        "# my custom rules\nfoo/\n"
    );
}

#[test]
fn init_refuses_if_jet_toml_exists() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("jet.toml"), "[package]\nname = \"x\"\n").unwrap();

    jet()
        .args(["init", "--no-vcs"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already has a jet.toml"));
}

#[test]
fn new_respects_java_version_flag() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("j25");

    jet()
        .args([
            "new",
            project.to_str().unwrap(),
            "--no-vcs",
            "--java",
            "25",
        ])
        .assert()
        .success();

    let manifest = fs::read_to_string(project.join("jet.toml")).unwrap();
    assert!(manifest.contains("java    = 25"));
}

#[test]
fn new_respects_name_override() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("dirname");

    jet()
        .args([
            "new",
            project.to_str().unwrap(),
            "--no-vcs",
            "--name",
            "actual-name",
        ])
        .assert()
        .success();

    let manifest = fs::read_to_string(project.join("jet.toml")).unwrap();
    assert!(manifest.contains("name    = \"actual-name\""));
    assert!(
        project
            .join("src/main/java/com/example/actual_name/Main.java")
            .is_file()
    );
}
