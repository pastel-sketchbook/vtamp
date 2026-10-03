//! Archive CLI/server integration, using private muted headless instances only.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

struct Server {
    home: tempfile::TempDir,
}
impl Server {
    fn new() -> Self {
        Self {
            home: tempfile::Builder::new()
                .prefix("vta-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .env("VTAMP_HOME", self.home.path())
            .env("PATH", self.home.path().join("no-optional-tools"))
            .env("VTAMP_MEDIA_KEYS", "0")
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.cmd(args);
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
    }
    fn start(&self) {
        self.ok(&["server", "start", "--headless"]);
        self.ok(&["volume", "0"]);
    }
    fn request(&self, command: Value) -> Value {
        let mut socket = UnixStream::connect(self.home.path().join("run/control.sock")).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let bytes = serde_json::to_vec(
            &json!({"version":vtamp::model::PROTOCOL_VERSION,"request":command}),
        )
        .unwrap();
        socket
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        socket.write_all(&bytes).unwrap();
        let mut length = [0; 4];
        socket.read_exact(&mut length).unwrap();
        let mut response = vec![0; u32::from_be_bytes(length) as usize];
        socket.read_exact(&mut response).unwrap();
        serde_json::from_slice(&response).unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.cmd(&["server", "stop"]);
    }
}

fn wav(path: &Path, seconds: u32) {
    let length = seconds * 48_000 * 2;
    let mut bytes = vec![];
    bytes.extend(b"RIFF");
    bytes.extend((36 + length).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(48_000u32.to_le_bytes());
    bytes.extend(96_000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(length.to_le_bytes());
    bytes.resize(44 + length as usize, 0);
    fs::write(path, bytes).unwrap();
}

#[test]
fn archive_cli_restores_without_tools_preserves_playback_and_survives_restart() {
    let source = Server::new();
    source.start();
    let music = source.home.path().join("music");
    fs::create_dir(&music).unwrap();
    wav(&music.join("one.wav"), 60);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a"),
        music.join("two.m4a"),
    )
    .unwrap();
    source.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    let tracks = source.ok(&["library", "list"]);
    let id = tracks["tracks"][0]["id"].as_str().unwrap();
    source.ok(&[
        "library",
        "edit",
        id,
        "--title",
        "김동률 Archive",
        "--album",
        "",
    ]);
    source.ok(&[
        "library",
        "stream",
        "add",
        "https://example.com/music",
        "--name",
        "Archive Radio",
    ]);
    let archive = source.home.path().join("library.tar.gz");
    let report = source.ok(&["library", "export", archive.to_str().unwrap()]);
    assert_eq!(report["included"], 2);
    assert_eq!(report["radios"], 1);

    let target = Server::new();
    let preview = target.ok(&["library", "import", archive.to_str().unwrap(), "--dry-run"]);
    assert_eq!(preview["added"], 2);
    assert!(!target.home.path().join("state.db").exists());
    assert!(!target.home.path().join("run").exists());
    target.start();
    let existing = target.home.path().join("playing.wav");
    wav(&existing, 120);
    target.ok(&["play", existing.to_str().unwrap()]);
    target.ok(&["pause"]);
    let before = target.ok(&["status"]);
    let started = target.request(json!({"command":"archive_import","path":archive}));
    assert_eq!(started["ok"], true);
    let job = started["data"]["job_id"].as_str().unwrap();
    // A catalog mutation cannot race the worker's snapshot/publication.
    let scan = target.request(json!({"command":"library_scan"}));
    assert_eq!(scan["error"]["code"], "library_busy");
    assert_eq!(target.ok(&["status"]), before);
    let mut completed = false;
    for _ in 0..200 {
        let status = target.ok(&["library", "archive-status", job]);
        assert!(status["progress"]["stage"].is_string(), "{status}");
        if status["status"] == "completed" {
            assert_eq!(status["added"], 2);
            assert_eq!(status["radios"], 1);
            completed = true;
            break;
        }
        assert_ne!(status["status"], "failed", "{status}");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(completed);
    assert_eq!(target.ok(&["status"]), before);
    assert_eq!(target.ok(&["library", "list"])["total"], 3);
    let repeat = target.ok(&["library", "import", archive.to_str().unwrap()]);
    assert_eq!(repeat["added"], 0);
    assert_eq!(repeat["duplicates"], 3);
    target.ok(&["library", "scan", "--wait"]);
    assert_eq!(
        target.ok(&["library", "search", "김동률"])["tracks"][0]["album"],
        ""
    );
    target.ok(&["server", "stop"]);
    target.start();
    assert_eq!(target.ok(&["library", "list"])["total"], 3);
    assert_eq!(target.ok(&["status"])["queue"], before["queue"]);
    assert_eq!(target.ok(&["status"])["position_ms"], before["position_ms"]);
    assert!(
        !target
            .cmd(&["library", "archive-status", job])
            .status
            .success()
    );
}

#[test]
fn archive_cli_export_works_offline_and_does_not_overwrite_output() {
    let server = Server::new();
    let archive = server.home.path().join("empty.tar.gz");
    let removed_option = server.cmd(&[
        "library",
        "export",
        archive.to_str().unwrap(),
        "--include-local",
    ]);
    assert_eq!(removed_option.status.code(), Some(2));
    assert!(!archive.exists());
    let exported = server.ok(&["library", "export", archive.to_str().unwrap()]);
    assert_eq!(exported["included"], 0);
    assert!(!server.home.path().join("state.db").exists());
    assert!(!server.home.path().join("run").exists());
    let before = fs::read(&archive).unwrap();
    assert!(
        !server
            .cmd(&["library", "export", archive.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(before, fs::read(archive).unwrap());
}

#[test]
fn archive_progress_uses_stderr_without_polluting_json_stdout() {
    let source = Server::new();
    source.start();
    let music = source.home.path().join("music");
    fs::create_dir(&music).unwrap();
    wav(&music.join("one.wav"), 1);
    source.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    let archive = source.home.path().join("progress.tar.gz");
    let exported = source.cmd(&["library", "export", archive.to_str().unwrap()]);
    assert!(exported.status.success());
    let data: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(data["data"]["included"], 1);
    let stderr = String::from_utf8(exported.stderr).unwrap();
    assert!(stderr.contains("Hashing"), "{stderr}");
    assert!(stderr.contains("Compressing"), "{stderr}");
    assert!(stderr.contains("1/1 files"), "{stderr}");
    assert!(stderr.contains("100%"), "{stderr}");
    assert!(!stderr.contains('\r'));
    assert!(!stderr.contains('\u{1b}'));

    let target = Server::new();
    let preview = target.cmd(&["library", "import", archive.to_str().unwrap(), "--dry-run"]);
    assert!(preview.status.success());
    let _: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(
        String::from_utf8(preview.stderr)
            .unwrap()
            .contains("Extracting and verifying")
    );
    assert!(!target.home.path().join("state.db").exists());
    target.start();
    let imported = target.cmd(&["library", "import", archive.to_str().unwrap()]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    assert!(
        String::from_utf8(imported.stderr)
            .unwrap()
            .contains("Import ·")
    );
    let data: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(data["data"]["progress"]["stage"], "completed");
}
