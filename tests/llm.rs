//! Generic LLM commands use private settings and fake providers, without downloads.
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_HOME", home)
        .env("VTAMP_MEDIA_KEYS", "0")
        .env("PATH", "/usr/bin:/bin")
        .args(args)
        .arg("--json")
        .output()
        .unwrap()
}
fn data(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
}
fn paths(home: &Path) -> vtamp::platform::Paths {
    vtamp::platform::Paths {
        data: home.into(),
        runtime: home.join("run"),
        cache: home.join("covers"),
    }
}

#[test]
fn llm_is_available_without_downloader_or_valid_import_settings() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    fs::write(
        home.join("imports.json"),
        b"unrelated invalid import settings",
    )
    .unwrap();
    let help = run(home, &["--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("llm"));
    assert!(!help.to_lowercase().contains("youtube"));
    assert!(!help.contains("metadata"));
    let status = data(run(home, &["llm", "status"]));
    assert_eq!(status["config"]["provider"], "none");
    assert_eq!(status["authentication_tested"], false);
    assert!(status.get("youtube").is_none());
    assert!(status.get("tools").is_none());
    assert_eq!(data(run(home, &["llm", "test"]))["tested"], false);
    assert!(!home.join("llm.json").exists());

    // Explicitly disabling the LLM must not read credentials or call an endpoint.
    let config = vtamp::llm::Config {
        endpoint: Some("invalid-unused-endpoint".into()),
        key_env: Some("VTAMP_UNUSED_TEST_KEY".into()),
        ..Default::default()
    };
    config.save(&paths(home)).unwrap();
    assert_eq!(data(run(home, &["llm", "test"]))["enabled"], false);
    assert_eq!(
        fs::read_to_string(home.join("imports.json")).unwrap(),
        "unrelated invalid import settings"
    );
    assert_eq!(
        fs::metadata(home.join("llm.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!home.join("state.db").exists());
    assert!(!home.join("run").exists());
}

#[test]
fn cli_connection_tests_send_only_generic_prompts_and_use_selected_tool() {
    let home = tempfile::tempdir().unwrap();
    for provider in [vtamp::llm::Provider::Codex, vtamp::llm::Provider::Claude] {
        let is_codex = provider == vtamp::llm::Provider::Codex;
        let exe = home.path().join("fake-cli");
        let output = if is_codex {
            json!({"ok":true})
        } else {
            json!({"structured_output":{"ok":true}})
        };
        fs::write(&exe, format!(r#"#!/usr/bin/python3
import sys,json,os
a=sys.argv[1:]
if '--help' in a:
 print('--output-schema --ephemeral --ignore-user-config --safe-mode --json-schema --tools');sys.exit(0)
if '--version' in a:
 print('fake-cli 1');sys.exit(0)
assert 'vtamp-llm-' in os.getcwd()
prompt=sys.stdin.read()
assert 'connection test' in prompt
assert not any(s in prompt.lower() for s in ['youtube','artist','song','metadata','video_id'])
if '--output-schema' in a:
 schema=json.load(open(a[a.index('--output-schema')+1]))
else:
 schema=json.loads(a[a.index('--json-schema')+1])
assert schema['required']==['ok']
print({})
"#, serde_json::to_string(&output.to_string()).unwrap())).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        vtamp::llm::Config {
            provider,
            executable: Some(exe),
            ..Default::default()
        }
        .save(&paths(home.path()))
        .unwrap();
        let status = data(run(home.path(), &["llm", "status"]));
        assert_eq!(status["tool"]["version"], "fake-cli 1");
        assert_eq!(status["authentication_tested"], false);
        let result = data(run(home.path(), &["llm", "test"]));
        assert_eq!(result["tested"], true);
        assert_eq!(result["response"], json!({"ok":true}));
        assert!(result.get("title").is_none());
        assert!(!home.path().join("imports.json").exists());
        assert!(!home.path().join("state.db").exists());
    }
}

#[test]
fn api_connection_test_is_generic_and_rejects_an_unexpected_reply() {
    for ok in [true, false] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 8192);
            }
            let header = String::from_utf8_lossy(&bytes).to_lowercase();
            assert!(header.starts_with("post /v1/chat/completions "));
            let length: usize = header
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "test-model");
            assert!(request.get("reasoning_effort").is_none());
            assert_eq!(
                request["response_format"]["json_schema"]["schema"]["required"],
                json!(["ok"])
            );
            let messages = request["messages"].to_string().to_lowercase();
            for word in ["youtube", "metadata", "artist", "song", "video_id"] {
                assert!(!messages.contains(word));
            }
            let response =
                json!({"choices":[{"message":{"content":json!({"ok":ok}).to_string()}}]})
                    .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        vtamp::llm::Config {
            provider: vtamp::llm::Provider::Api,
            endpoint: Some(format!("http://{address}/v1")),
            model: Some("test-model".into()),
            ..Default::default()
        }
        .save(&paths(home.path()))
        .unwrap();
        let output = run(home.path(), &["llm", "test"]);
        assert_eq!(
            output.status.success(),
            ok,
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        server.join().unwrap();
    }
}
