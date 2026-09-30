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
