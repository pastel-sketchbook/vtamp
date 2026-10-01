//! Isolated process tests with fake download tools; no network, cookies or audio device.
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
    time::{Duration, Instant},
};
struct Harness {
    home: tempfile::TempDir,
}
impl Harness {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("vti-")
            .tempdir_in("/tmp")
            .unwrap();
        let bin = home.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
        image::RgbImage::from_pixel(16, 9, image::Rgb([40, 120, 180]))
            .save(bin.join("art.png"))
            .unwrap();
        let script = format!(
            r#"#!/usr/bin/python3
import sys,json,pathlib,shutil,time
args=sys.argv[1:]
base=pathlib.Path(__file__).parent
if '--version' in args:
 print('fake 1');sys.exit(0)
with open(base/'calls','a') as f: f.write(json.dumps(args)+'\n')
url=args[-1]
video=url.split('v=')[-1]
if (base/'slow').exists(): time.sleep(60)
if '--flat-playlist' in args:
 print(json.dumps({{'title':'Test playlist','entries':[{{'id':'lO3lG-qXU14','title':'one'}},{{'id':'SECOND00001','title':'two'}},{{'id':'FAILED00001','title':'missing'}}]}}));sys.exit(0)
if video=='FAILED00001' and not (base/'repair').exists():
 print('Video unavailable',file=sys.stderr);sys.exit(1)
if '--dump-single-json' in args:
 print(json.dumps({{'id':video,'title':"이승환 + 정준일 '어떻게 사랑이 그래요'",'channel':'이승환 LEE SEUNG HWAN','channel_id':'UCtest','live_status':'not_live','album':(base/'album').read_text() if (base/'album').exists() else None}}));sys.exit(0)
assert '-f' in args and args[args.index('-f')+1]=='bestaudio[ext=m4a]/bestaudio'
assert '--cookies-from-browser' not in args
out=pathlib.Path(args[args.index('-o')+1].replace('%(ext)s','m4a'))
shutil.copyfile({fixture},out)
shutil.copyfile(base/'art.png',out.with_suffix('.png'))
print('VTAMP_PROGRESS '+json.dumps({{'downloaded_bytes':50,'total_bytes':100,'speed':1024,'eta':1}}),flush=True)
time.sleep(.15)
print('VTAMP_PROGRESS '+json.dumps({{'downloaded_bytes':100,'total_bytes':100,'speed':1024,'eta':0}}),flush=True)
print('VTAMP_FILE '+json.dumps(str(out)))
"#,
            fixture = serde_json::to_string(&fixture).unwrap()
        );
        fs::write(bin.join("yt-dlp"), script).unwrap();
        fs::write(bin.join("ffmpeg"),"#!/usr/bin/python3\nimport sys,shutil\na=sys.argv[1:]\nif '-i' in a: shutil.copyfile(a[a.index('-i')+1],a[-1])\nelse: print('fake 1')\n").unwrap();
        for name in ["yt-dlp", "ffmpeg"] {
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(home.path().join("imports.json"),serde_json::to_vec(&json!({"youtube":{"yt_dlp":bin.join("yt-dlp"),"ffmpeg":bin.join("ffmpeg"),"ffprobe":bin.join("ffmpeg")}})).unwrap()).unwrap();
        Self { home }
    }
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .env("VTAMP_HOME", self.home.path())
            .env("VTAMP_MEDIA_KEYS", "0")
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let o = self.cmd(args);
        assert!(
            o.status.success(),
            "{} {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()["data"].clone()
    }
    fn wait(&self, id: &str) -> Value {
        let start = Instant::now();
        loop {
            let v = self.ok(&["library", "import-status", id]);
            if v["job"]["finished_at_ms"].is_number() {
                return v;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "{v}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cmd(&["server", "stop"]);
        for _ in 0..150 {
            if !self.home.path().join("run/control.sock").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
const URL: &str = "https://www.youtube.com/watch?v=lO3lG-qXU14";
#[test]
fn single_import_metadata_cover_dedup_edits_and_rescan() {
    let h = Harness::new();
    let preview = h.ok(&["library", "add", URL, "--preview"]);
    assert_eq!(preview["metadata"]["artist"], "이승환, 정준일");
    assert!(!h.home.path().join("run/control.sock").exists());
    assert!(!h.home.path().join("state.db").exists());
    let result = h.ok(&["library", "add", URL, "--wait", "--timeout", "15s"]);
    assert_eq!(result["job"]["added"], 1);
    assert_eq!(result["job"]["progress"]["bytes"], 100);
    assert_eq!(result["job"]["progress"]["eta"], Value::Null);
    let tracks = h.ok(&["library", "list"]);
    let t = &tracks["tracks"][0];
    let id = t["id"].as_str().unwrap();
    assert_eq!(result["job"]["first_added_track_id"], id);
    assert_eq!(t["title"], "어떻게 사랑이 그래요");
    assert_eq!(t["artist"], "이승환, 정준일");
    assert_eq!(t["album"], "Generated fixtures"); // Fall back to the embedded album.
    assert_eq!(t["source"]["video_id"], "lO3lG-qXU14");
    let cover = image::open(t["cover"].as_str().unwrap()).unwrap();
    assert_eq!((cover.width(), cover.height()), (512, 512));
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    h.ok(&[
        "library",
        "edit",
        id,
        "--title",
        "My title",
        "--artist",
        "My artist",
    ]);
    let after = h.ok(&["status"]);
    assert_eq!(after["queue_revision"], before["queue_revision"]);
    assert_eq!(after["queue"][0]["track"]["title"], "My title");
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "track", id])["title"], "My title");
    h.ok(&["library", "retag", id]);
    assert_eq!(h.ok(&["library", "track", id])["artist"], "My artist");
    let duplicate = h.ok(&["library", "add", URL, "--wait"]);
    assert_eq!(duplicate["job"]["skipped"], 1);
    assert!(duplicate["job"]["first_added_track_id"].is_null());
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start"]);
    assert_eq!(
        h.ok(&["library", "track", id])["source"]["video_id"],
        "lO3lG-qXU14"
    );
}
#[test]
fn playlist_partial_failure_and_retry_only_unfinished() {
    let h = Harness::new();
    let j = h.ok(&["library", "add", "https://youtube.com/playlist?list=PLtest"]);
    let id = j["job_id"].as_str().unwrap();
    let result = h.wait(id);
    assert_eq!(result["job"]["status"], "partial");
    assert_eq!(result["job"]["added"], 2);
    assert_eq!(result["job"]["failed"], 1);
    assert_eq!(
        result["job"]["first_added_track_id"],
        result["items"][0]["track_id"]
    );
    assert_ne!(
        result["job"]["first_added_track_id"],
        result["items"][1]["track_id"]
    );
    assert_eq!(
        h.ok(&[
            "library",
            "import-status",
            id,
            "--offset",
            "1",
            "--limit",
            "1"
        ])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(h.home.path().join("bin/repair"), b"").unwrap();
    let retry = h.ok(&["library", "import-retry", id]);
    let r = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(r["job"]["added"], 1);
    assert_eq!(r["job"]["total"], 1);
    assert_eq!(r["job"]["first_added_track_id"], r["items"][0]["track_id"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 3);
}
#[test]
fn cancellation_keeps_server_responsive_and_stops_child() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    let j = h.ok(&["library", "add", URL]);
    let id = j["job_id"].as_str().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    h.ok(&["volume", "31"]);
    assert!(t.elapsed() < Duration::from_secs(2));
    h.ok(&["library", "import-cancel", id]);
    assert_eq!(h.wait(id)["job"]["status"], "cancelled");
    fs::remove_file(h.home.path().join("bin/slow")).unwrap();
    let retry = h.ok(&["library", "import-retry", id]);
    assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);
}
#[test]
fn absent_downloader_keeps_optional_features_out_of_help() {
    let h = Harness::new();
    fs::remove_file(h.home.path().join("bin/yt-dlp")).unwrap();
    for args in [
        vec!["--help"],
        vec!["library", "--help"],
        vec!["library", "add", "--help"],
    ] {
        let o = h.cmd(&args);
        let text = String::from_utf8_lossy(&o.stdout).to_lowercase();
        assert!(o.status.success());
        for word in [
            "youtube",
            "yt-dlp",
            "playlist",
            "clipboard",
            "import-status",
        ] {
            assert!(!text.contains(word), "{word}: {text}");
        }
    }
    assert!(!h.home.path().join("run/control.sock").exists());
    h.ok(&["server", "start"]);
    let doctor = h.ok(&["doctor"]);
    assert!(doctor.get("imports").is_none());
}

#[test]
fn imports_use_shared_llm_settings_and_keep_rule_fallback() {
    let h = Harness::new();
    fs::write(
        h.home.path().join("llm.json"),
        serde_json::to_vec(&json!({
            "provider":"api", "endpoint":"invalid-endpoint", "model":"test-model"
        }))
        .unwrap(),
    )
    .unwrap();
    let result = h.ok(&["library", "add", URL, "--wait"]);
    assert_eq!(result["job"]["added"], 1);
    assert!(
        result["items"][0]["metadata"]["warning"]
            .as_str()
            .unwrap()
            .contains("LLM unavailable")
    );
    assert_eq!(result["items"][0]["metadata"]["method"], "rules");
}

#[test]
fn failed_catalog_commit_recovers_published_audio_without_redownload() {
    let h = Harness::new();
    h.ok(&["server", "start"]);
    let db = rusqlite::Connection::open(h.home.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_import BEFORE INSERT ON tracks BEGIN SELECT RAISE(FAIL,'injected catalog failure'); END;").unwrap();
    let started = h.ok(&["library", "add", URL]);
    let id = started["job_id"].as_str().unwrap();
    assert_eq!(h.wait(id)["job"]["status"], "failed");
    assert_eq!(h.ok(&["library", "list"])["total"], 0);
    assert!(
        h.home
            .path()
            .join("imports/youtube/lO3lG-qXU14/audio.m4a")
            .is_file()
    );
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    db.execute_batch("DROP TRIGGER fail_import;").unwrap();
    let retry = h.ok(&["library", "import-retry", id]);
    assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);
    assert_eq!(
        calls,
        fs::read_to_string(h.home.path().join("bin/calls")).unwrap()
    );
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
}

#[test]
fn shutdown_interrupts_jobs_and_restart_requires_explicit_retry() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    let j = h.ok(&["library", "add", URL]);
    let id = j["job_id"].as_str().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let now = Instant::now();
    h.ok(&["server", "stop"]);
    // Wait for the old socket's cleanup before starting a new listener.
    while h.home.path().join("run/control.sock").exists() {
        assert!(now.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::remove_file(h.home.path().join("bin/slow")).unwrap();
    h.ok(&["server", "start"]);
    assert_eq!(
        h.ok(&["library", "import-status", id])["job"]["status"],
        "interrupted"
    );
    assert_eq!(h.ok(&["library", "list"])["total"], 0);
    let retry = h.ok(&["library", "import-retry", id]);
    assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);
}

#[test]
fn missing_managed_file_is_repaired_with_same_track_identity_and_overrides() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/album"), "Source album").unwrap();
    h.ok(&["library", "add", URL, "--wait", "--title", "Remember me"]);
    let tracks = h.ok(&["library", "list"]);
    let t = &tracks["tracks"][0];
    assert_eq!(t["album"], "Source album");
    h.ok(&["library", "edit", t["id"].as_str().unwrap(), "--album", ""]);
    fs::remove_file(t["path"].as_str().unwrap()).unwrap();
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 0);
    h.ok(&["library", "add", URL, "--wait"]);
    let repaired = h.ok(&["library", "list"]);
    assert_eq!(repaired["tracks"][0]["id"], t["id"]);
    assert_eq!(repaired["tracks"][0]["title"], "Remember me");
    assert_eq!(repaired["tracks"][0]["album"], "");
}

#[test]
fn album_override_and_clear_survive_rescans_retagging_and_restart() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/album"), "Source album").unwrap();
    h.ok(&["library", "add", URL, "--wait"]);
    let tracks = h.ok(&["library", "list"]);
    let track = &tracks["tracks"][0];
    let id = track["id"].as_str().unwrap();
    assert_eq!(track["album"], "Source album");
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    for (input, expected) in [("  My album  ", "My album"), ("   ", "")] {
        h.ok(&["library", "edit", id, "--album", input]);
        // Change mtime so the scan must re-read the file and source manifest.
        fs::File::options()
            .write(true)
            .open(track["path"].as_str().unwrap())
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
            .unwrap();
        h.ok(&["library", "scan", "--wait"]);
        h.ok(&["library", "retag", id]);
        assert_eq!(h.ok(&["library", "track", id])["album"], expected);
        let after = h.ok(&["status"]);
        assert_eq!(after["queue"][0]["track"]["album"], expected);
        for field in [
            "queue_revision",
            "current_id",
            "position_ms",
            "status",
            "volume",
        ] {
            assert_eq!(after[field], before[field]);
        }
        assert_eq!(after["queue"][0]["id"], before["queue"][0]["id"]);
        if expected.is_empty() {
            assert_eq!(
                h.ok(&["library", "search", "--album", "My album", "--exact"])["total"],
                0
            );
        } else {
            assert_eq!(
                h.ok(&["library", "search", "--album", expected, "--exact"])["total"],
                1
            );
        }
    }
    let invalid = h.cmd(&["library", "edit", id, "--album", "bad\nvalue"]);
    assert!(!invalid.status.success());
    assert_eq!(h.ok(&["library", "track", id])["album"], "");
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start"]);
    assert_eq!(h.ok(&["library", "track", id])["album"], "");
    assert_eq!(h.ok(&["status"])["queue"][0]["track"]["album"], "");
}

#[tokio::test]
async fn watchers_receive_queued_job_cancellation_before_active_job_finishes() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    h.ok(&["server", "start"]);
    let paths = vtamp::platform::Paths {
        data: h.home.path().into(),
        runtime: h.home.path().join("run"),
        cache: h.home.path().join("covers"),
    };
    let (_, mut stream) = vtamp::client::Client::new(paths).watch().await.unwrap();
    let running = h.ok(&["library", "add", URL]);
    let queued = h.ok(&["library", "add", URL]);
    let id = queued["job_id"].as_str().unwrap();
    h.ok(&["library", "import-cancel", id]);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let reply: vtamp::model::Reply = vtamp::wire::read(&mut stream).await.unwrap();
            let value = reply.into_data().unwrap();
            if value["event"] == "imports"
                && value["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|j| j["job_id"] == id && j["status"] == "cancelled")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    h.ok(&[
        "library",
        "import-cancel",
        running["job_id"].as_str().unwrap(),
    ]);
    assert_eq!(
        h.wait(running["job_id"].as_str().unwrap())["job"]["status"],
        "cancelled"
    );
}

#[test]
fn queued_jobs_use_the_configuration_captured_when_submitted() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    let first = h.ok(&["library", "add", URL]);
    std::thread::sleep(Duration::from_millis(150));
    let queued = h.ok(&["library", "add", URL, "--title", "Captured settings"]);
    fs::write(
        h.home.path().join("imports.json"),
        b"invalid changed settings",
    )
    .unwrap();
    fs::write(
        h.home.path().join("llm.json"),
        b"invalid changed LLM settings",
    )
    .unwrap();
    fs::remove_file(h.home.path().join("bin/slow")).unwrap();
    h.ok(&[
        "library",
        "import-cancel",
        first["job_id"].as_str().unwrap(),
    ]);
    let completed = h.wait(queued["job_id"].as_str().unwrap());
    assert_eq!(completed["job"]["status"], "completed");
    assert_eq!(
        h.ok(&["library", "list"])["tracks"][0]["title"],
        "Captured settings"
    );
}
