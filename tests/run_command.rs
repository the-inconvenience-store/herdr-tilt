#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;

#[test]
fn run_command_starts_tilt_and_records_the_session() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let state = temp.path().join("state");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(project.join("Tiltfile"), "").unwrap();

    let capture = temp.path().join("tilt-args");
    let fake_tilt = temp.path().join("tilt");
    fs::write(
        &fake_tilt,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_tilt, fs::Permissions::from_mode(0o755)).unwrap();

    let context = serde_json::json!({
        "workspace_id": "w1",
        "workspace_cwd": project,
        "focused_pane_cwd": project,
    });

    Command::cargo_bin("herdr-tilt")
        .unwrap()
        .arg("run")
        .env("TILT_BIN_PATH", &fake_tilt)
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_PLUGIN_CONTEXT_JSON", context.to_string())
        .env("CAPTURE", &capture)
        .assert()
        .success();

    let args = fs::read_to_string(capture).unwrap();
    let args = args.lines().collect::<Vec<_>>();
    assert_eq!(args[0..3], ["up", "--stream", "--port"]);
    assert!(args[3].parse::<u16>().is_ok());
    assert_eq!(args[4], "-f");
    assert_eq!(
        args[5],
        project
            .join("Tiltfile")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );

    let records = fs::read_dir(state.join("sessions"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), 1);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(records[0].path()).unwrap()).unwrap();
    assert_eq!(record["phase"], "exited");
    assert_eq!(record["exit_code"], 0);
    assert_eq!(record["port"], args[3].parse::<u16>().unwrap());
}
