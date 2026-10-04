use std::{
    fs,
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};

pub fn process_exists(pid: u32) -> bool {
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

pub struct Driver {
    pub child: Child,
    pub dir: PathBuf,
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
    pub fn log(&self) -> String {
        fs::read_to_string(self.dir.join("watch.log")).unwrap()
    }
    pub fn wait_for(&mut self, marker: &str) {
        self.wait_for_timeout(marker, Duration::from_secs(180));
    }
    pub fn wait_for_timeout(&mut self, marker: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
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
