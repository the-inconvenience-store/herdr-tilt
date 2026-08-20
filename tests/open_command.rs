#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;

#[test]
fn open_command_opens_status_split_for_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(project.join("Tiltfile"), "").unwrap();

    let capture = temp.path().join("herdr-args");
    let fake_herdr = temp.path().join("herdr");
    fs::write(
        &fake_herdr,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE\"\nprintf '%s\\n' '{\"result\":{\"plugin_pane\":{\"pane\":{\"pane_id\":\"w1:p9\"}}}}'\n",
    )
    .unwrap();
    fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o755)).unwrap();

    let context = serde_json::json!({
        "workspace_id": "w1",
        "workspace_cwd": project,
        "focused_pane_id": "w1:p1",
        "focused_pane_cwd": project,
    });

    Command::cargo_bin("herdr-tilt")
        .unwrap()
        .arg("open")
        .env("HERDR_BIN_PATH", &fake_herdr)
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_PLUGIN_CONTEXT_JSON", context.to_string())
        .env("CAPTURE", &capture)
        .assert()
        .success();

    let args = fs::read_to_string(capture).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr.tilt",
            "--entrypoint",
            "status",
            "--placement",
            "split",
            "--direction",
            "right",
            "--workspace",
            "w1",
            "--target-pane",
            "w1:p1",
            "--cwd",
            project.canonicalize().unwrap().to_str().unwrap(),
            "--focus",
        ]
    );
}

#[test]
fn repeated_open_focuses_the_saved_plugin_pane() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(project.join("Tiltfile"), "").unwrap();

    let capture = temp.path().join("herdr-calls");
    let fake_herdr = temp.path().join("herdr");
    fs::write(
        &fake_herdr,
        "#!/bin/sh\nprintf '%s' \"$1 $2 $3\" >> \"$CAPTURE\"\nshift 3\nprintf ' %s' \"$@\" >> \"$CAPTURE\"\nprintf '\\n' >> \"$CAPTURE\"\nif [ \"$1\" = 'open' ]; then :; fi\nprintf '%s\\n' '{\"result\":{\"plugin_pane\":{\"pane\":{\"pane_id\":\"w1:p9\"}}}}'\n",
    )
    .unwrap();
    fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o755)).unwrap();

    let context = serde_json::json!({
        "workspace_id": "w1",
        "workspace_cwd": project,
        "focused_pane_id": "w1:p1",
        "focused_pane_cwd": project,
    });
    for _ in 0..2 {
        Command::cargo_bin("herdr-tilt")
            .unwrap()
            .arg("open")
            .env("HERDR_BIN_PATH", &fake_herdr)
            .env("HERDR_PLUGIN_STATE_DIR", &state)
            .env("HERDR_PLUGIN_CONTEXT_JSON", context.to_string())
            .env("CAPTURE", &capture)
            .assert()
            .success();
    }

    let calls = fs::read_to_string(capture).unwrap();
    let calls = calls.lines().collect::<Vec<_>>();
    assert!(calls[0].starts_with("plugin pane open "));
    assert_eq!(calls[1], "plugin pane focus w1:p9");
}
