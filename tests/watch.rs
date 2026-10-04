#![cfg(target_os = "linux")]

use std::{
    fs,
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};

#[test]
fn rejects_dx_and_conflicting_targets() {
    for args in [
        vec!["watch", "--dx", "dx"],
        vec!["watch", "--bin", "ui", "--example", "demo"],
        vec!["watch", "--debounce-ms", "0"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn rejects_internal_compiler_commands_outside_watch() {
    let output = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .arg("__link")
        .env_remove("GPUI_DRIVER_SESSION")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a watch session"));
}

struct Driver {
    child: Child,
    dir: PathBuf,
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        let _ = self.child.wait();
        // Keep failed-test logs and source available for diagnosis.
    }
}
impl Driver {
    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("watch.log")).unwrap()
    }
    fn wait_for(&mut self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline {
            let log = self.log();
            if log.contains(marker) {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("driver exited {status}:\n{log}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("waiting for {marker:?}:\n{}", self.log());
    }
}

#[test]
#[ignore = "builds a real Subsecond app and binds a local websocket server"]
fn patches_without_dx_recovers_from_incomplete_source_and_cleans_up() {
    let dir = std::env::temp_dir().join(format!("gpui-native-e2e-{}", std::process::id()));
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        include_str!("fixtures/hot-patch/Cargo.toml"),
    )
    .unwrap();
    fs::create_dir_all(dir.join("helper/src")).unwrap();
    fs::write(
        dir.join("helper/Cargo.toml"),
        include_str!("fixtures/hot-patch/helper/Cargo.toml"),
    )
    .unwrap();
    fs::write(
        dir.join("helper/src/lib.rs"),
        include_str!("fixtures/hot-patch/helper/src/lib.rs"),
    )
    .unwrap();
    let original = include_str!("fixtures/hot-patch/src/main.rs");
    fs::write(dir.join("src/main.rs"), original).unwrap();
    let log = fs::File::create(dir.join("watch.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_gpui"))
        .args(["watch", "--offline", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(["--", "hello world", "$(literal)"])
        .env(
            "CARGO_TARGET_DIR",
            std::env::temp_dir().join("gpui-native-e2e-cache"),
        )
        .env("GPUI_DX", "/this-backend-does-not-exist")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut driver = Driver { child, dir };
    driver.wait_for("Watching gpui-native-probe");
    driver.wait_for("revision=before");
    assert!(
        driver
            .log()
            .contains("args=[\"hello world\", \"$(literal)\"]")
    );
    let source = driver.dir.join("src/main.rs");
    fs::write(&source, "fn main( {").unwrap();
    driver.wait_for("Waiting for valid Rust source");
    fs::write(
        &source,
        original.replace("revision=before", "revision=after"),
    )
    .unwrap();
    driver.wait_for("revision=after");
    // A type error must preserve the old process and recover on the next save.
    fs::write(
        &source,
        original
            .replace("revision=before", "revision=broken")
            .replace("count += 1", "count += MISSING_VALUE"),
    )
    .unwrap();
    driver.wait_for("Patch failed:");
    fs::write(
        &source,
        original.replace("revision=before", "revision=recovered"),
    )
    .unwrap();
    driver.wait_for("revision=recovered");
    let log = driver.log();
    assert_eq!(
        log.lines().filter(|l| l.starts_with("START pid=")).count(),
        1,
        "{log}"
    );
    let pid: u32 = log
        .lines()
        .find_map(|line| line.strip_prefix("START pid="))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let counts: Vec<u64> = log
        .lines()
        .filter(|l| l.starts_with("revision="))
        .map(|line| {
            assert!(line.contains(&format!("pid={pid} ")), "{line}");
            line.rsplit("count=").next().unwrap().parse().unwrap()
        })
        .collect();
    assert!(counts.windows(2).all(|w| w[0] < w[1]));
    // Failed full rebuilds must also keep the current process running.
    let manifest = driver.dir.join("Cargo.toml");
    let manifest_text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &source,
        original.replace("count += 1", "count += MISSING_VALUE"),
    )
    .unwrap();
    fs::write(
        &manifest,
        manifest_text.replace(
            "version = \"0.1.0\"",
            "version = \"0.1.0\"\ndescription = \"restart probe\"",
        ),
    )
    .unwrap();
    driver.wait_for("Rebuild failed:");
    assert!(PathBuf::from(format!("/proc/{pid}")).exists());
    fs::write(
        &source,
        original.replace("revision=before", "revision=restarted"),
    )
    .unwrap();
    driver.wait_for("Restarted gpui-native-probe");
    driver.wait_for("revision=restarted");
    let log = driver.log();
    let next_pid: u32 = log
        .lines()
        .filter_map(|line| line.strip_prefix("START pid="))
        .next_back()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(pid, next_pid);
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("START pid="))
            .count(),
        2
    );
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    fs::write(
        &source,
        original.replace("revision=before", "revision=post_restart"),
    )
    .unwrap();
    driver.wait_for("revision=post_restart");
    // A path dependency nested inside the app directory must rebuild too.
    let helper = driver.dir.join("helper/src/lib.rs");
    fs::write(
        &helper,
        fs::read_to_string(&helper)
            .unwrap()
            .replace("helper_before", "helper_after"),
    )
    .unwrap();
    driver.wait_for("helper=helper_after");
    let final_pid: u32 = driver
        .log()
        .lines()
        .filter_map(|line| line.strip_prefix("START pid="))
        .next_back()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_ne!(next_pid, final_pid);
    assert!(!PathBuf::from(format!("/proc/{next_pid}")).exists());
    drop(driver);
    assert!(!PathBuf::from(format!("/proc/{final_pid}")).exists());
    assert!(!PathBuf::from(format!("/proc/{next_pid}")).exists());
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "application survived driver shutdown"
    );
}
