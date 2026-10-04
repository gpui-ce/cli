use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "gpui-new-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn creates_an_app_with_an_explicit_name_in_a_path_with_spaces() {
    let root = root();
    let path = root.join("app with spaces");
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .arg("new")
        .arg(&path)
        .args(["--name", "counter-app"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(path.join("Cargo.toml"))
        .output()
        .unwrap();
    assert!(
        metadata.status.success(),
        "{}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    assert_eq!(metadata["packages"][0]["name"], "counter-app");
    let dependencies = metadata["packages"][0]["dependencies"].as_array().unwrap();
    assert!(dependencies.iter().any(|dep| {
        dep["name"] == "gpui-ce"
            && dep["features"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("hot-patching"))
    }));
    assert!(path.join("src/main.rs").is_file());
    assert!(path.join("LICENSE").is_file());
    assert!(
        fs::read_to_string(path.join("README.md"))
            .unwrap()
            .starts_with("# counter-app")
    );
    // Existing files must survive even if a second creation uses another name.
    let source = path.join("src/main.rs");
    fs::write(&source, "user-owned content").unwrap();
    let retry = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .arg("new")
        .arg(&path)
        .args(["--name", "another-name"])
        .output()
        .unwrap();
    assert!(!retry.status.success());
    assert_eq!(fs::read_to_string(&source).unwrap(), "user-owned content");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_invalid_names_without_creating_directories() {
    let root = root();
    for name in [
        "",
        "123app",
        "app name",
        "fn",
        "self",
        "gen",
        "bad\"name",
        "../escape",
    ] {
        let path = root.join("new-app");
        let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
            .arg("new")
            .arg(&path)
            .arg("--name")
            .arg(name)
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {name:?}");
        assert!(!path.exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn derives_the_package_name_from_the_directory() {
    let root = root();
    let path = root.join("my-counter");
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .arg("new")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        fs::read_to_string(path.join("Cargo.toml"))
            .unwrap()
            .contains("name = \"my-counter\"")
    );
    fs::remove_dir_all(root).unwrap();
}
