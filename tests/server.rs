use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    process::{Command, Output, Stdio},
    time::Duration,
};

struct Server {
    home: tempfile::TempDir,
}
impl Server {
    fn new() -> Self {
        Self {
            home: tempfile::Builder::new()
                .prefix("vtamp-test-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .env("VTAMP_HOME", self.home.path())
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.cmd(args);
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
    }
    fn socket(&self) -> std::path::PathBuf {
        self.home.path().join("run/control.sock")
    }
    fn wait_stopped(&self) {
        for _ in 0..100 {
            if !self.socket().exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("Server failed to clean up its socket");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.cmd(&["server", "stop"]);
        self.wait_stopped();
    }
}
fn send(stream: &mut UnixStream, value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}
fn receive(stream: &mut UnixStream) -> Value {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn concurrent_start_watch_and_restore() {
    let server = Server::new();
    assert!(!server.cmd(&["status"]).status.success());
    assert!(
        !server.socket().exists(),
        "Read-only status must not start a server"
    );
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    server.ok(&["server", "start"]);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let mut watch = UnixStream::connect(server.socket()).unwrap();
    watch
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut watch,
        json!({"version":1, "request":{"command":"watch"}}),
    );
    assert_eq!(receive(&mut watch)["ok"], true);
    server.ok(&["volume", "42"]);
    loop {
        let event = receive(&mut watch);
        if event["data"]["event"] == "state" && event["data"]["data"]["volume"] == 42 {
            break;
        }
    }
    drop(watch);
    server.ok(&["repeat", "all"]);
    server.ok(&["server", "stop"]);
    server.wait_stopped();
    server.ok(&["server", "start"]);
    let state = server.ok(&["status"]);
    assert_eq!(state["volume"], 42);
    assert_eq!(state["repeat"], "all");
    assert_eq!(state["status"], "stopped");
}

#[test]
fn bad_protocol_does_not_damage_server() {
    let server = Server::new();
    server.ok(&["server", "start"]);
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut stream,
        json!({"version":999, "request":{"command":"status"}}),
    );
    assert_eq!(receive(&mut stream)["error"]["code"], "version_mismatch");
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
    assert_eq!(receive(&mut stream)["error"]["code"], "invalid_request");
    server.ok(&["pause"]);
    let invalid = server.cmd(&["volume", "101"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&invalid.stdout).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[test]
fn crash_leaves_socket_that_next_launch_recovers() {
    let server = Server::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_HOME", server.home.path())
        .args(["server", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if server.cmd(&["status"]).status.success() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    server.ok(&["status"]);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(server.socket().exists());
    server.ok(&["server", "start"]);
    server.ok(&["status"]);
}
