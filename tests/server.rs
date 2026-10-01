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
            .env("VTAMP_MEDIA_KEYS", "0")
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
        json!({"version":vtamp::model::PROTOCOL_VERSION, "request":{"command":"watch"}}),
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
        .env("VTAMP_MEDIA_KEYS", "0")
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

#[test]
fn agent_cli_search_scan_queue_and_timer_contracts() {
    let server = Server::new();
    for args in [
        vec!["now"],
        vec!["sleep", "status"],
        vec!["library", "track", "missing"],
        vec!["queue", "list", "--limit", "1"],
    ] {
        assert!(!server.cmd(&args).status.success());
        assert!(!server.socket().exists());
    }
    let music = server.home.path().join("music");
    std::fs::create_dir(&music).unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav"),
        music.join("Love.wav"),
    )
    .unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav"),
        music.join("Love live.wav"),
    )
    .unwrap();
    std::fs::write(music.join("broken.wav"), b"bad audio").unwrap();
    let scan = server.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    assert_eq!(scan["status"], "completed");
    assert_eq!(scan["summary"]["added"], 2);
    assert_eq!(scan["summary"]["warning_count"], 1);
    assert!(
        scan["summary"]["warnings"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("broken.wav")
    );
    assert_eq!(
        server.ok(&["library", "scan-status", scan["job_id"].as_str().unwrap()]),
        scan
    );
    let search = server.ok(&["library", "search", "--title", "love", "--exclude", "live"]);
    assert_eq!(search["total"], 1);
    let id = search["tracks"][0]["id"].as_str().unwrap();
    assert_eq!(server.ok(&["library", "track", id])["title"], "Love");
    assert_eq!(
        server.ok(&["library", "search", "--title", "LOVE", "--exact"])["total"],
        1
    );
    let now = server.ok(&["now"]);
    assert!(now["current"].is_null());
    assert!(now.get("queue").is_none());
    let added = server.ok(&[
        "queue",
        "add",
        "--tracks",
        id,
        id,
        "--after-current",
        "--if-queue-revision",
        "0",
        "--request-id",
        "batch",
    ]);
    assert_eq!(added["changes"][0]["items"].as_array().unwrap().len(), 2);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 2);
    let page = server.ok(&["queue", "list", "--offset", "1", "--limit", "1"]);
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let edit = server.home.path().join("edit.json");
    std::fs::write(
        &edit,
        serde_json::to_vec(
            &json!({"operations":[{"op":"remove","queue_item_ids":[page["items"][0]["id"]]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    let before = server.ok(&["status"]);
    assert_eq!(
        server.ok(&[
            "queue",
            "edit",
            "--file",
            edit.to_str().unwrap(),
            "--dry-run"
        ])["applied"],
        false
    );
    assert_eq!(server.ok(&["status"]), before);
    let conflict = server.cmd(&[
        "queue",
        "edit",
        "--file",
        edit.to_str().unwrap(),
        "--if-queue-revision",
        "0",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stdout).unwrap()["error"]["code"],
        "queue_conflict"
    );
    server.ok(&["queue", "edit", "--file", edit.to_str().unwrap()]);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
    // Replaying an old success must not undo later edits or trip its old guard.
    assert_eq!(
        server.ok(&[
            "queue",
            "add",
            "--tracks",
            id,
            id,
            "--after-current",
            "--if-queue-revision",
            "0",
            "--request-id",
            "batch"
        ]),
        added
    );
    server.ok(&["sleep", "set", "30m"]);
    assert_eq!(
        server.ok(&["sleep", "status"])["scheduled_stop"]["kind"],
        "deadline"
    );
    server.ok(&["server", "stop"]);
    server.ok(&["server", "start"]);
    assert!(server.ok(&["sleep", "status"])["scheduled_stop"].is_null());
    assert_eq!(
        server.ok(&[
            "queue",
            "add",
            "--tracks",
            id,
            id,
            "--after-current",
            "--if-queue-revision",
            "0",
            "--request-id",
            "batch"
        ]),
        added
    );
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
    let second = server.ok(&["library", "scan", "--wait"]);
    assert_eq!(second["summary"]["unchanged"], 2);
    server.ok(&["sleep", "set", "1s"]);
    std::thread::sleep(Duration::from_millis(1150));
    assert!(server.ok(&["now"])["scheduled_stop"].is_null());
}

#[test]
fn concurrent_and_disconnected_batch_requests_apply_once() {
    let server = Server::new();
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    server.ok(&["library", "add", fixtures, "--wait"]);
    let list = server.ok(&["library", "list"]);
    let id = list["tracks"][0]["id"].as_str().unwrap();
    let request = json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{
        "command":"queue_edit","edit":{"operations":[{"op":"add","track_ids":[id]}]},
        "dry_run":false,"if_queue_revision":0,"request_id":"disconnected"}});
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    send(&mut stream, request.clone());
    drop(stream); // The caller loses its response, not the accepted edit.
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    let mut stream = UnixStream::connect(server.socket()).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    send(&mut stream, request.clone());
                    let result = receive(&mut stream);
                    assert_eq!(result["ok"], true, "{result}");
                    result
                })
            })
            .collect();
        let replies: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert!(replies.windows(2).all(|w| w[0] == w[1]));
    });
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn spectrum_is_a_separate_read_only_latest_frame_stream() {
    let server = Server::new();
    server.ok(&["server", "start"]);
    let before = server.ok(&["status"]);
    let mut watchers = Vec::new();
    for _ in 0..3 {
        let mut stream = UnixStream::connect(server.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        send(
            &mut stream,
            json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"spectrum_watch"}}),
        );
        let first = receive(&mut stream);
        assert_eq!(first["ok"], true);
        assert_eq!(first["data"]["levels"].as_array().unwrap().len(), 32);
        assert_eq!(first["data"]["active"], false);
        assert!(first["data"].get("queue").is_none());
        watchers.push(stream);
    }
    // Ordinary watch may include import snapshots, but never spectrum frames.
    let mut ordinary = UnixStream::connect(server.socket()).unwrap();
    ordinary
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut ordinary,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"watch"}}),
    );
    assert_eq!(receive(&mut ordinary)["data"], before);
    let mut event = receive(&mut ordinary);
    if event["data"]["event"] == "imports" {
        assert_eq!(event["data"]["data"], json!([]));
        event = receive(&mut ordinary);
    }
    assert_eq!(event["data"]["event"], "progress");
    let frame = receive(&mut watchers[0]);
    assert!(
        frame["data"]["levels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.as_f64() == Some(0.0))
    );
    assert_eq!(server.ok(&["status"]), before);
    drop(watchers);
    assert!(server.ok(&["now"]).get("levels").is_none());
}
