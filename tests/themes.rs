use serde_json::Value;
use std::process::{Command, Output};

fn run(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_MEDIA_KEYS", "0")
        .env("VTAMP_HOME", home)
        .args(args)
        .arg("--json")
        .output()
        .unwrap()
}
fn data(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
}

#[test]
fn theme_cli_is_persistent_machine_readable_and_server_independent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    assert_eq!(
        data(run(home, &["theme", "list"]))["themes"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    assert_eq!(
        data(run(home, &["theme", "current"]))["theme"],
        "catppuccin-mocha"
    );
    assert!(!home.join("ui.json").exists());
    assert_eq!(
        data(run(home, &["theme", "set", "rose-pine"]))["applies_to"],
        "future_attachments"
    );
    assert_eq!(data(run(home, &["theme", "current"]))["theme"], "rose-pine");
    let invalid = run(home, &["theme", "set", "unknown"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&invalid.stdout).unwrap()["error"]["code"],
        "invalid_arguments"
    );
    assert_eq!(data(run(home, &["theme", "current"]))["theme"], "rose-pine");
    std::fs::write(home.join("ui.json"), "bad").unwrap();
    assert_eq!(run(home, &["theme", "current"]).status.code(), Some(1));
    // Explicit set repairs bad preferences; listing does not depend on them.
    data(run(home, &["theme", "list"]));
    data(run(home, &["theme", "set", "nord"]));
    assert_eq!(data(run(home, &["theme", "current"]))["theme"], "nord");
    assert!(!home.join("run").exists());
    assert!(!home.join("state.db").exists());
}

#[test]
fn custom_theme_install_select_repair_and_warnings_are_server_independent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/pastel");
    let mut files = std::fs::read_dir(examples)
        .unwrap()
        .map(|e| e.unwrap().path().to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    files.sort();
    let args = ["theme", "install"]
        .into_iter()
        .chain(files.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let report = data(run(&home, &args));
    assert_eq!(report["installed"].as_array().unwrap().len(), 16);
    assert!(report["warnings"].as_array().unwrap().is_empty());
    assert!(!home.join("ui.json").exists());
    assert_eq!(
        data(run(&home, &["theme", "list"]))["themes"]
            .as_array()
            .unwrap()
            .len(),
        25
    );
    let original = br#"{"theme":"nord","spectrum":true,"spectrum_style":"sparks","video":false}"#;
    std::fs::write(home.join("ui.json"), original).unwrap();
    assert_eq!(
        data(run(&home, &args))["unchanged"]
            .as_array()
            .unwrap()
            .len(),
        16
    );
    assert_eq!(std::fs::read(home.join("ui.json")).unwrap(), original);
    data(run(&home, &["theme", "set", "pastel-default"]));
    assert_eq!(
        data(run(&home, &["theme", "current"]))["theme"],
        "pastel-default"
    );
    let current: Value =
        serde_json::from_slice(&std::fs::read(home.join("ui.json")).unwrap()).unwrap();
    assert_eq!(current["spectrum"], true);
    assert_eq!(current["spectrum_style"], "sparks");
    assert_eq!(current["video"], false);
    // Attachment-only overrides validate dynamic IDs without touching preferences.
    data(run(
        &home,
        &["--theme", "pastel-default-light", "theme", "current"],
    ));
    assert_eq!(run(&home, &["--theme", "missing"]).status.code(), Some(2));
    let saved = std::fs::read(home.join("ui.json")).unwrap();
    std::fs::write(home.join("themes/pastel-default.json"), "bad").unwrap();
    assert_eq!(run(&home, &["theme", "current"]).status.code(), Some(1));
    assert_eq!(
        run(&home, &["theme", "set", "pastel-default"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(std::fs::read(home.join("ui.json")).unwrap(), saved);
    let listed = data(run(&home, &["theme", "list"]));
    assert_eq!(listed["themes"].as_array().unwrap().len(), 24);
    assert!(
        listed["warnings"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("pastel-default.json")
    );
    std::fs::remove_file(home.join("themes/pastel-default.json")).unwrap();
    assert_eq!(run(&home, &["theme", "current"]).status.code(), Some(1));
    data(run(&home, &["theme", "set", "nord"]));
    let repaired: Value =
        serde_json::from_slice(&std::fs::read(home.join("ui.json")).unwrap()).unwrap();
    assert_eq!(repaired["video"], false);
    assert_eq!(repaired["spectrum_style"], "sparks");
    assert!(!home.join("state.db").exists());
    assert!(!home.join("run").exists());
}

#[test]
fn arbitrary_theme_file_is_discovered_without_registration_or_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    std::fs::create_dir(&themes).unwrap();
    std::fs::write(
        themes.join("my-music.json"),
        include_str!("../themes/pastel/pastel-default.json"),
    )
    .unwrap();
    let listed = data(run(dir.path(), &["theme", "list"]));
    assert_eq!(listed["themes"][9]["id"], "my-music");
    data(run(dir.path(), &["theme", "set", "my-music"]));
    assert_eq!(
        data(run(dir.path(), &["theme", "current"]))["theme"],
        "my-music"
    );
    assert!(!dir.path().join("state.db").exists());
}
