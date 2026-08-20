#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use herdr_tilt::project::Project;
use herdr_tilt::session::{SessionPhase, SessionRecord, record_path};
use herdr_tilt::tilt::CircleStatus;
use herdr_tilt::tui::{DashboardModel, OverallStatus};

#[test]
fn dashboard_without_tiltfile_warns_and_disables_controls() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let project = Project {
        root: workspace.path().to_path_buf(),
        tiltfile: None,
    };

    let model = DashboardModel::new(project, state.path().to_path_buf());

    assert_eq!(model.overall_status(), OverallStatus::Unavailable);
    assert_eq!(model.warning(), Some("No Tiltfile found in this workspace"));
    assert!(!model.can_start());
    assert!(!model.can_stop());
}

#[test]
fn running_dashboard_refreshes_services_from_tilt() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let tiltfile = tiltfile.canonicalize().unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.clone()),
    };
    let path = record_path(state.path(), &tiltfile);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&SessionRecord {
            tiltfile,
            project_root: project.root.clone(),
            port: 41234,
            runner_pid: 100,
            tilt_pid: 101,
            started_unix_ms: 1234,
            phase: SessionPhase::Running,
            exit_code: None,
        })
        .unwrap(),
    )
    .unwrap();

    let fake_tilt = workspace.path().join("tilt");
    fs::write(
        &fake_tilt,
        r#"#!/bin/sh
printf '%s\n' '{"items":[{"metadata":{"name":"api"},"status":{"order":1,"updateStatus":"ok","runtimeStatus":"ok"}}]}'
"#,
    )
    .unwrap();
    fs::set_permissions(&fake_tilt, fs::Permissions::from_mode(0o755)).unwrap();

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();

    assert_eq!(model.overall_status(), OverallStatus::Running);
    assert_eq!(model.services.len(), 1);
    assert_eq!(model.services[0].name, "api");
    assert_eq!(model.services[0].status, CircleStatus::Green);
    assert!(model.can_stop());
}

#[test]
fn dashboard_start_invokes_the_retained_herdr_action() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.canonicalize().unwrap()),
    };
    let capture = workspace.path().join("herdr-args");
    let fake_herdr = workspace.path().join("herdr");
    fs::write(
        &fake_herdr,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            capture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_herdr, fs::Permissions::from_mode(0o755)).unwrap();

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.start_with_herdr(&fake_herdr).unwrap();

    assert_eq!(
        fs::read_to_string(capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["plugin", "action", "invoke", "herdr.tilt.run"]
    );
    assert_eq!(model.overall_status(), OverallStatus::Starting);
}
