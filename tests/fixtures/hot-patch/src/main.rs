use std::{thread, time::Duration};

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFAbsoluteTimeGetCurrent() -> f64;
}

fn render(count: u64) {
    #[cfg(target_os = "macos")]
    println!("FRAMEWORK time={}", unsafe { CFAbsoluteTimeGetCurrent() });
    println!("helper={}", gpui_native_helper::label());
    println!("revision=before pid={} count={count}", std::process::id());
}

fn main() {
    dioxus_devtools::connect_subsecond();
    let mut count = 0u64;
    println!("START pid={} args={:?}", std::process::id(), std::env::args().skip(1).collect::<Vec<_>>());
    loop {
        count += 1;
        subsecond::HotFn::current(render).call((count,));
        thread::sleep(Duration::from_millis(100));
    }
}
