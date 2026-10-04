#![cfg(unix)]

use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(version: &str, body: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gpui-watch-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = 'fixture'\n").unwrap();
        let backend = dir.join("fake dx");
        fs::write(
            &backend,
            format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'dioxus {version}'; exit 0; fi\n{body}\n"),
        ).unwrap();
        fs::set_permissions(backend, fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_gpui"))
            .args(["watch", "--dx"])
            .arg(self.0.join("fake dx"))
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn forwards_build_options_and_argument_boundaries() {
    let fixture = Fixture::new("0.7.10", "printf '%s\\n' \"$@\"");
    let output = fixture.run(&[
        "-p",
        "my-app",
        "--bin",
        "ui",
        "-F",
        "first,second",
        "--features",
        "third",
        "--no-default-features",
        "--all-features",
        "--locked",
        "--offline",
        "--args",
        "hello world; $(not-a-command)",
    ]);
    assert!(output.status.success(), "{:?}", output);
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines,
        [
            "serve",
            "--hot-patch",
            "--watch=true",
            "--open=false",
            "--package",
            "my-app",
            "--bin",
            "ui",
            "--args",
            "hello world; $(not-a-command)",
            "--features",
            "first,second third",
            "--all-features",
            "--no-default-features",
            "--cargo-args=--locked --offline",
        ]
    );
}

#[test]
fn runs_in_manifest_directory_and_resolves_relative_backend() {
    let fixture = Fixture::new("0.7.10", "pwd");
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .current_dir(&fixture.0)
        .args([
            "watch",
            "--dx",
            "./fake dx",
            "--manifest-path",
            "Cargo.toml",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        fixture.0.canonicalize().unwrap().to_str().unwrap()
    );
}

#[test]
fn preserves_backend_exit_code_and_signal() {
    let fixture = Fixture::new("0.7.10", "exit 42");
    assert_eq!(fixture.run(&[]).status.code(), Some(42));
    let fixture = Fixture::new("0.7.10", "kill -TERM $$");
    assert_eq!(fixture.run(&[]).status.signal(), Some(15));
}

#[test]
fn rejects_incompatible_backend_before_running_it() {
    let fixture = Fixture::new("0.7.9", "echo unexpectedly-started");
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("requires dioxus-cli 0.7.10")
    );
}

#[test]
fn missing_backend_has_install_instructions() {
    let fixture = Fixture::new("0.7.10", "exit 0");
    fs::remove_file(fixture.0.join("fake dx")).unwrap();
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cargo install dioxus-cli --version 0.7.10 --locked")
    );
}

#[test]
fn rejects_conflicting_targets_and_invalid_manifests() {
    let fixture = Fixture::new("0.7.10", "echo unexpectedly-started");
    assert_eq!(
        fixture
            .run(&["--bin", "ui", "--example", "demo"])
            .status
            .code(),
        Some(2)
    );
    let output = fixture.run(&["--manifest-path", fixture.0.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("must point to a Cargo.toml")
    );
}
