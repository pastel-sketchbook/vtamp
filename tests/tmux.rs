use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Output},
    time::{Duration, Instant},
};

fn run(home: &std::path::Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_MEDIA_KEYS", "0")
        .env("VTAMP_HOME", home)
        .args(["tmux", "status"])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn absent_server_is_quiet_and_does_not_create_runtime_files() {
    let home = tempfile::tempdir().unwrap();
    let output = run(home.path(), &[]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\n");
    assert!(output.stderr.is_empty());
    let output = run(home.path(), &["--json"]);
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        reply,
        json!({"version": 1, "ok": true, "data": {"text": ""}})
    );
    assert!(!home.path().join("run").exists());
    assert!(!home.path().join("state.db").exists());
    assert!(!run(home.path(), &["--max-width", "5"]).status.success());
}

fn fake_server<F: FnOnce(std::os::unix::net::UnixStream) + Send>(
    respond: F,
    extra: &[&str],
) -> (Output, Duration) {
    let home = tempfile::Builder::new()
        .prefix("vtamp-tmux-")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir(home.path().join("run")).unwrap();
    let socket = UnixListener::bind(home.path().join("run/control.sock")).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let (mut stream, _) = socket.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            let mut payload = vec![0; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut payload).unwrap();
            let request: Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(request["request"]["command"], "status");
            respond(stream);
        });
        let started = Instant::now();
        let output = run(home.path(), extra);
        (output, started.elapsed())
    })
}

#[test]
fn unresponsive_server_does_not_stall_a_status_job() {
    let (output, elapsed) = fake_server(
        |_stream| std::thread::sleep(Duration::from_millis(1200)),
        &[],
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\n");
    assert!(output.stderr.is_empty());
    assert!(elapsed < Duration::from_millis(1000), "{elapsed:?}");
}

#[test]
fn renders_existing_protocol_without_starting_or_mutating_playback() {
    for (status, prefix) in [("playing", "▶"), ("paused", "Ⅱ"), ("stopped", "")] {
        let (output, _) = fake_server(
            |mut stream| {
                let reply = json!({"version": 1, "ok": true, "data": {
                    "status": status, "position_ms": 56_000, "current_id": "entry",
                    "queue": [{"id": "entry", "track": {
                        "id": "track", "path": "/music/song.m4a", "title": "Training Montage",
                        "artist": "Vince DiCola", "album": "The Rocky Story", "track_number": 1,
                        "duration_ms": 219_000, "cover": null
                    }}]
                }});
                let bytes = serde_json::to_vec(&reply).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            },
            &["--json"],
        );
        assert!(output.status.success());
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        let expected = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix} Training Montage · 0:56 / 3:39")
        };
        assert_eq!(reply["data"]["text"], expected);
    }
}
