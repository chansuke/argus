//! The `argusd` binary: it reads its settings, serves the socket, and refuses bad settings.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "argusd-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `argusd` with nothing from this environment but a home directory.
fn argusd(home: &std::path::Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_argusd"));
    c.args(args)
        .env_clear()
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    c
}

#[test]
fn argusd_serves_the_api_on_the_socket_its_settings_name() {
    let dir = scratch();
    let socket = dir.join("s.sock");
    let config = dir.join("config.json");
    let settings = format!(
        r#"{{"data_root": "{}", "socket": "{}", "skill_dirs": []}}"#,
        dir.join("data").display(),
        socket.display()
    );
    std::fs::write(&config, settings).unwrap();
    let mut child = argusd(&dir, &["--config", config.to_str().unwrap()])
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            break stream;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("argusd exited: {status}");
        }
        assert!(Instant::now() < deadline, "argusd never served");
        std::thread::sleep(Duration::from_millis(50));
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut w = &stream;
    w.write_all(b"{\"kind\":\"Hello\",\"token\":null,\"run\":null}\n")
        .unwrap();
    w.write_all(b"{\"kind\":\"Call\",\"id\":1,\"op\":\"presets\",\"input\":{}}\n")
        .unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    // Only `argusd` itself is stopped: one pid, never a group.
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(reply.contains(r#""kind":"Reply""#), "{reply}");
    // The default preset, as no preset was set.
    assert!(reply.contains("opus"), "{reply}");
    assert!(dir.join("data").is_dir());
}

#[test]
fn argusd_refuses_bad_settings_and_says_which() {
    let dir = scratch();
    let config = dir.join("config.json");
    std::fs::write(&config, r#"{"phone": {"listen": "0.0.0.0:7420"}}"#).unwrap();
    let out = argusd(&dir, &["--config", config.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("phone.listen"), "{err}");

    let out = argusd(&dir, &["--verbose"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}
