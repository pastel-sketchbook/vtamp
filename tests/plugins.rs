use serde_json::{Value, json};
use std::{fs, path::Path, process::Command, sync::Arc, time::Duration};
use vtamp::{
    model::State,
    platform::Paths,
    plugin::{self, Context, Session, Target, Update},
};

fn paths(home: &Path) -> Paths {
    Paths {
        data: home.into(),
        cache: home.join("covers"),
        runtime: home.join("run"),
    }
}

fn manifest(home: &Path, script: &str) -> std::path::PathBuf {
    let file = home.join("plugin.json");
    let script_path = home.join("plugin.sh");
    fs::write(&script_path, script).unwrap();
    fs::write(
        &file,
        serde_json::to_vec(&json!({
            "api_version":1,"id":"fixture","name":"Fixture",
            "exec":["/bin/sh",script_path],
            "commands":[{"id":"show","title":"Show fixture","target":"none"}]
        }))
        .unwrap(),
    )
    .unwrap();
    file
}

fn session(home: &Path, script: &str) -> Session {
    let plugin = plugin::read_manifest(&manifest(home, script)).unwrap();
    let command = plugin.manifest.commands[0].clone();
    Session::start(
        plugin,
        command,
        Context::from_state(&State::default(), false, Target::None, None),
        paths(home),
        Arc::default(),
    )
}

async fn finished(session: &mut Session) -> Arc<Update> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let update = session.updates.borrow_and_update().clone();
            if update.finished {
                return update;
            }
            session.updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

const READY: &str =
    "read -r init\nprintf '%s\\n' '{\"type\":\"ready\",\"api_version\":1}'\nread -r invoke\n";
const VIEW: &str = "printf '%s\\n' '{\"type\":\"view\",\"generation\":1,\"view\":{\"title\":\"Fixture\",\"items\":[{\"text\":\"Hello 가사\",\"start_ms\":0}]}}'\n";

#[test]
fn registry_is_read_only_until_explicit_registration_and_never_starts_server() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .args(args)
            .arg("--json")
            .env("VTAMP_HOME", &home)
            .env("VTAMP_MEDIA_KEYS", "0")
            .output()
            .unwrap()
    };
    let output = run(&["plugin", "list"]);
    assert!(output.status.success());
    assert!(!home.exists());
    let file = manifest(dir.path(), "exit 99");
    assert!(
        run(&["plugin", "add", file.to_str().unwrap()])
            .status
            .success()
    );
    let listed: Value = serde_json::from_slice(&run(&["plugin", "list"]).stdout).unwrap();
    assert_eq!(listed["data"]["plugins"][0]["manifest"]["id"], "fixture");
    assert!(!home.join("run").exists());
    assert!(!home.join("state.db").exists());
    assert!(!home.join("plugins").exists());
    let failed = run(&["plugin", "run", "fixture:show"]);
    assert!(!failed.status.success());
    assert!(!home.join("run").exists());
    assert!(!home.join("state.db").exists());
    assert!(run(&["plugin", "remove", "fixture"]).status.success());
    let registry: Value =
        serde_json::from_slice(&fs::read(home.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(registry["plugins"], json!({}));
}

#[test]
fn invalid_plugins_and_reserved_bindings_do_not_hide_valid_commands() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let file = manifest(dir.path(), "exit 0");
    plugin::Registry::add(&paths, &file).unwrap();
    fs::write(
        dir.path().join("plugins.json"),
        serde_json::to_vec(&json!({
            "plugins":{"fixture":file,"missing":dir.path().join("missing.json")},
            "bindings":{"L":"fixture:show","q":"fixture:show",":":"fixture:show","?":"fixture:show"}
        }))
        .unwrap(),
    )
    .unwrap();
    let catalog = plugin::Catalog::load(&paths);
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.bindings.len(), 1);
    assert_eq!(catalog.bindings[&'L'], "fixture:show");
    assert_eq!(catalog.warnings.len(), 4);
}

#[tokio::test]
async fn protocol_delivers_view_and_done_without_a_playback_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = session(
        dir.path(),
        &format!("{READY}{VIEW}printf '%s\\n' '{{\"type\":\"done\",\"generation\":1}}'\n"),
    );
    let update = finished(&mut session).await;
    assert!(!update.error, "{}", update.notice);
    assert_eq!(update.view.as_ref().unwrap().items[0].text, "Hello 가사");
    session.close().await;
    assert!(!dir.path().join("run").exists());
    assert!(!dir.path().join("state.db").exists());
}

#[tokio::test]
async fn invalid_handshake_output_and_oversize_messages_are_isolated() {
    for script in [
        "read -r init; printf '%s\\n' '{\"type\":\"ready\",\"api_version\":99}'".to_owned(),
        format!("{READY}printf '%s\\n' 'not json'"),
        format!("{READY}head -c 1048577 /dev/zero"),
        "exit 3".to_owned(),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut session = session(dir.path(), &script);
        let update = finished(&mut session).await;
        assert!(update.error);
        session.close().await;
    }
}

#[tokio::test]
async fn closing_reaps_descendants_even_when_the_plugin_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!(
        "{READY}sleep 60 &\nprintf '%s' $! > '{}'\nwait\n",
        pid_file.display()
    );
    let session = session(dir.path(), &script);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(2), session.close())
        .await
        .unwrap();
    // A killed orphan may briefly remain a zombie; ps distinguishes it from a live process.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let output = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            if state.trim().is_empty() || state.trim().starts_with('Z') {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn old_generations_are_ignored_and_actions_are_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "{READY}{VIEW}while read -r message; do\ncase \"$message\" in\n*'\"type\":\"action\"'*) printf '%s\\n' '{{\"type\":\"view\",\"generation\":1,\"view\":{{\"title\":\"Old\",\"items\":[]}}}}' '{{\"type\":\"view\",\"generation\":2,\"view\":{{\"title\":\"New\",\"items\":[]}}}}' '{{\"type\":\"done\",\"generation\":2}}'; exit 0;;\nesac\ndone\n"
    );
    let mut session = session(dir.path(), &script);
    tokio::time::timeout(Duration::from_secs(3), async {
        while session.updates.borrow().view.is_none() {
            session.updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let mut context = Context::from_state(&State::default(), false, Target::None, None);
    context.generation = 2;
    session.context(context);
    session.action("refresh".into(), 2).unwrap();
    let update = finished(&mut session).await;
    assert!(!update.error, "{}", update.notice);
    assert_eq!(update.view.as_ref().unwrap().title, "New");
    session.close().await;
}

#[tokio::test]
async fn stalled_handshake_has_a_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = session(dir.path(), "sleep 60");
    let update = finished(&mut session).await;
    assert!(update.error && update.notice.contains("handshake timed out"));
    session.close().await;
}

#[test]
fn context_invalidates_track_identity_but_not_progress_or_pause() {
    let mut state = State::default();
    let mut context = Context::from_state(&state, true, Target::None, None);
    state.position_ms = 100;
    assert!(!context.advance(Context::from_state(&state, true, Target::None, None)));
    assert!(context.advance(Context::from_state(&state, false, Target::None, None)));
    assert_eq!(context.generation, 1);
}

#[test]
fn playing_context_tracks_entry_identity_and_selected_context_stays_pinned() {
    use vtamp::model::{PlaybackStatus, QueueItem, Track};
    let track: Track = serde_json::from_value(json!({
        "id":"first","path":"/test/song.wav","title":"First","artist":"Artist","album":"",
        "track_number":0,"duration_ms":10000,"cover":null
    }))
    .unwrap();
    let mut state = State::default();
    state.queue.push(QueueItem::new(track.clone()));
    state.current_id = Some(state.queue[0].id.clone());
    state.status = PlaybackStatus::Playing;
    let mut playing = Context::from_state(&state, true, Target::Playing, None);
    let pinned = Context::from_state(&state, true, Target::Selected, Some(&track));
    state.position_ms = 4000;
    state.status = PlaybackStatus::Paused;
    assert!(playing.advance(Context::from_state(&state, true, Target::Playing, None)));
    assert_eq!(playing.generation, 1);
    assert_eq!(playing.position_ms, 4000);
    state.queue.push(QueueItem::new(track.clone()));
    state.current_id = Some(state.queue[1].id.clone());
    playing.advance(Context::from_state(&state, true, Target::Playing, None));
    assert_eq!(playing.generation, 2);
    state.queue[1].track.id = "second".into();
    state.queue[1].track.title = "Second".into();
    let selected = Context::from_state(&state, true, Target::Selected, Some(&track));
    assert_eq!(selected.track, pinned.track);
    assert_eq!(selected.playback_id, None);
    assert_eq!(selected.position_ms, 0);
}

#[tokio::test]
async fn errors_after_a_track_change_do_not_restore_an_obsolete_view() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!("{READY}{VIEW}read -r context\nprintf '%s\\n' broken\n");
    let mut session = session(dir.path(), &script);
    tokio::time::timeout(Duration::from_secs(3), async {
        while session.updates.borrow().view.is_none() {
            session.updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let mut context = Context::from_state(&State::default(), false, Target::None, None);
    context.generation = 2;
    session.context(context);
    let update = finished(&mut session).await;
    assert!(update.error);
    assert_eq!(update.generation, 2);
    assert!(update.view.is_none());
    session.close().await;
}

#[tokio::test]
async fn independent_sessions_and_headless_cli_keep_their_own_lifetimes() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first = session(first_dir.path(), &format!("{READY}{VIEW}sleep 60"));
    let script = format!(
        "{READY}{VIEW}while read -r message; do\ncase \"$message\" in\n*'\"type\":\"action\"'*) printf '%s\\n' '{{\"type\":\"done\",\"generation\":1}}'; exit 0;;\nesac\ndone"
    );
    let mut second = session(second_dir.path(), &script);
    tokio::time::timeout(Duration::from_secs(3), async {
        while second.updates.borrow().view.is_none() {
            second.updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    first.close().await;
    second.action("retry".into(), 1).unwrap();
    assert!(!finished(&mut second).await.error);
    second.close().await;

    let home = second_dir.path().join("cli");
    let file = manifest(
        second_dir.path(),
        &format!("{READY}{VIEW}printf '%s\\n' '{{\"type\":\"done\",\"generation\":1}}'"),
    );
    plugin::Registry::add(&paths(&home), &file).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .args(["plugin", "run", "fixture:show", "--json"])
        .env("VTAMP_HOME", &home)
        .env("VTAMP_MEDIA_KEYS", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message["data"]["view"]["title"] == "Fixture")
    );
    assert_eq!(messages.last().unwrap()["data"]["finished"], true);
    assert!(!home.join("run").exists());
    assert!(!home.join("state.db").exists());
}
