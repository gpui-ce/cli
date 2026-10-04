#![cfg(any(target_os = "linux", target_os = "macos"))]

mod support;
use std::{fs, process::Command, time::Duration};
use support::{Driver, process_exists};

#[test]
#[ignore = "opens a real GPUI window; requires a display and GPUI's native build dependencies"]
fn generated_app_opens_a_window_and_preserves_state_across_render_patches() {
    let dir = std::env::temp_dir().join(format!("gpui-window-e2e-{}", std::process::id()));
    let created = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .arg("new")
        .arg(&dir)
        .args(["--name", "gpui-window-probe"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    // CI uses a checkout of the template's pinned revision. Locally, the git
    // dependencies work too; GPUI_SOURCE can select an existing checkout.
    if let Some(source) = std::env::var_os("GPUI_SOURCE") {
        let source = fs::canonicalize(source).unwrap();
        let manifest = dir.join("Cargo.toml");
        let mut text = fs::read_to_string(&manifest).unwrap();
        let git = "git = \"https://github.com/gpui-ce/gpui-ce\", rev = \"96a5ecca405b288a0107eaa1c057d954db6a41be\"";
        let gpui = serde_json::to_string(&source.join("crates/gpui")).unwrap();
        let platform = serde_json::to_string(&source.join("crates/gpui_platform")).unwrap();
        text = text
            .lines()
            .map(|line| {
                if line.starts_with("gpui = ") {
                    line.replace(git, &format!("path = {gpui}"))
                } else if line.starts_with("gpui_ce_platform = ") {
                    line.replace(git, &format!("path = {platform}"))
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(manifest, text).unwrap();
    }
    let source = dir.join("src/main.rs");
    let original = fs::read_to_string(&source).unwrap();
    assert!(original.contains("        div()"));
    assert!(original.contains("Counter { count: 0 }"));
    let original = original.replace("Counter { count: 0 }", "Counter { count: 42 }")
        .replace("fn main() {", "fn main() {\n    eprintln!(\"GPUI_WINDOW_START pid={}\", std::process::id());")
        .replacen("        div()", "        eprintln!(\"GPUI_WINDOW_RENDER revision=before pid={} count={}\", std::process::id(), self.count);\n        div()", 1);
    fs::write(&source, &original).unwrap();
    let log = fs::File::create(dir.join("watch.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .args(["watch", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .env("GPUI_DX", "/this-backend-does-not-exist")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut driver = Driver { child, dir };
    driver.wait_for_timeout("Watching gpui-window-probe", Duration::from_secs(900));
    driver.wait_for("GPUI_WINDOW_RENDER revision=before");
    let pid: u32 = driver
        .log()
        .lines()
        .find_map(|line| line.strip_prefix("GPUI_WINDOW_START pid="))
        .unwrap()
        .parse()
        .unwrap();
    for revision in ["after", "second"] {
        let patch = original
            .replace("revision=before", &format!("revision={revision}"))
            .replace("Counter { count: 42 }", "Counter { count: 9001 }")
            .replace("0x20242b", "0x245534");
        fs::write(&source, patch).unwrap();
        driver.wait_for(&format!(
            "GPUI_WINDOW_RENDER revision={revision} pid={pid} count=42"
        ));
    }
    let log = driver.log();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("GPUI_WINDOW_START"))
            .count(),
        1,
        "{log}"
    );
    assert!(process_exists(pid));
    drop(driver);
    assert!(
        !process_exists(pid),
        "GPUI window survived watcher shutdown"
    );
}
