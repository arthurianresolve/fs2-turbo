use std::io::Write as _;
use std::time::Duration;

// Test-only executable. It ignores Cargo arguments and never invokes a build.
fn main() {
    println!("fixture stdout");
    eprintln!("fixture stderr");
    std::io::stdout().flush().unwrap();
    if let Some(path) = std::env::var_os("FS2_DEV_CARGO_FIXTURE_RECEIPT") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    match std::env::var("FS2_DEV_CARGO_FIXTURE_MODE").as_deref() {
        Ok("exit") => std::process::exit(7),
        Ok("wait") => {
            // Bound this fixture even if the containment implementation regresses.
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(99);
        }
        #[cfg(unix)]
        Ok("group-wait") => wait_with_group_member(),
        #[cfg(unix)]
        Ok("group-reaper") => reap_group_member(),
        #[cfg(unix)]
        Ok("group-member") => {
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(96);
        }
        #[cfg(unix)]
        Ok("signal") => terminate_by_signal(),
        _ => std::process::exit(97),
    }
}

#[cfg(unix)]
fn wait_with_group_member() -> ! {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let ready = std::path::PathBuf::from(std::env::var_os("FS2_DEV_CARGO_FIXTURE_READY").unwrap());
    let mut reaper = Command::new(std::env::current_exe().unwrap());
    reaper
        .env("FS2_DEV_CARGO_FIXTURE_MODE", "group-reaper")
        .env(
            "FS2_DEV_CARGO_FIXTURE_GROUP",
            std::process::id().to_string(),
        )
        .env("FS2_DEV_CARGO_FIXTURE_READY", &ready)
        .env_remove("FS2_DEV_CARGO_FIXTURE_RECEIPT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();

    let started = Instant::now();
    while !ready.exists() {
        if started.elapsed() >= Duration::from_secs(2) {
            std::process::exit(95);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_secs(30));
    std::process::exit(94);
}

#[cfg(unix)]
fn reap_group_member() -> ! {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let process_group = std::env::var("FS2_DEV_CARGO_FIXTURE_GROUP")
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let ready = std::env::var_os("FS2_DEV_CARGO_FIXTURE_READY").unwrap();
    let mut member = Command::new(std::env::current_exe().unwrap());
    let mut member = member
        .env("FS2_DEV_CARGO_FIXTURE_MODE", "group-member")
        .env_remove("FS2_DEV_CARGO_FIXTURE_RECEIPT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(process_group)
        .spawn()
        .unwrap();
    std::fs::write(ready, member.id().to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let _ = member.kill();
    let _ = member.wait();
    std::process::exit(0);
}

#[cfg(unix)]
fn terminate_by_signal() -> ! {
    unsafe extern "C" {
        fn raise(signal: std::ffi::c_int) -> std::ffi::c_int;
    }
    let signal = std::env::var("FS2_DEV_CARGO_FIXTURE_SIGNAL")
        .unwrap()
        .parse::<std::ffi::c_int>()
        .unwrap();
    // SAFETY: The test supplies this platform's SIGTERM and raises it only in
    // this fixture process. No other process or process group is signalled.
    unsafe { raise(signal) };
    std::process::exit(98);
}
