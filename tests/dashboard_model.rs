#![cfg(unix)]

use std::fs;

use herdr_tilt::project::Project;
use herdr_tilt::session::{SessionPhase, SessionRecord, record_path};
use herdr_tilt::tilt::{CircleStatus, ServiceAction};
use herdr_tilt::tui::{DashboardModel, OverallStatus};

mod support;

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
        format!(
            r#"#!/bin/sh
if [ "$2" = "sessions" ]; then
  printf '%s\n' '{{"items":[{{"spec":{{"tiltfilePath":"{}"}},"status":{{"pid":101,"startTime":"2026-08-20T01:02:03Z"}}}}]}}'
elif [ "$2" = "uibuttons" ]; then
  printf '%s\n' '{{"items":[{{"metadata":{{"name":"api-seed"}},"spec":{{"text":"Seed","location":{{"componentID":"api","componentType":"Resource"}}}}}}]}}'
else
  printf '%s\n' '{{"items":[{{"metadata":{{"name":"api"}},"status":{{"order":1,"updateStatus":"ok","runtimeStatus":"ok","endpointLinks":[{{"name":"API","url":"https://api.test"}}]}}}}]}}'
fi
"#,
            project.tiltfile.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    support::publish_executable(&fake_tilt);

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();

    assert_eq!(model.overall_status(), OverallStatus::Running);
    assert_eq!(model.services.len(), 1);
    assert_eq!(model.services[0].name, "api");
    assert_eq!(model.services[0].status, CircleStatus::Green);
    assert!(matches!(
        model.services[0].actions[0],
        ServiceAction::Link { .. }
    ));
    assert!(matches!(
        model.services[0].actions[1],
        ServiceAction::Button { .. }
    ));
    assert!(!model.can_start());
    assert!(model.can_stop());
}

#[test]
fn dashboard_discovers_manually_started_tilt_on_the_default_port() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.canonicalize().unwrap()),
    };
    let fake_tilt = workspace.path().join("tilt");
    fs::write(
        &fake_tilt,
        format!(
            r#"#!/bin/sh
if [ "$5" != "--port" ] || [ "$6" != "10350" ]; then
  exit 9
fi
if [ "$2" = "sessions" ]; then
  printf '%s\n' '{{"items":[{{"spec":{{"tiltfilePath":"{}"}},"status":{{"pid":321,"startTime":"2026-08-20T01:02:03Z"}}}}]}}'
else
  printf '%s\n' '{{"items":[{{"metadata":{{"name":"manual-api"}},"status":{{"order":1,"updateStatus":"ok","runtimeStatus":"ok"}}}}]}}'
fi
"#,
            project.tiltfile.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    support::publish_executable(&fake_tilt);

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();

    assert_eq!(model.overall_status(), OverallStatus::Running);
    assert_eq!(model.services.len(), 1);
    assert_eq!(model.services[0].name, "manual-api");
    assert!(!model.can_start());
}

#[test]
fn dashboard_without_a_default_tilt_api_remains_stopped() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.canonicalize().unwrap()),
    };
    let fake_tilt = workspace.path().join("tilt");
    fs::write(&fake_tilt, "#!/bin/sh\nexit 1\n").unwrap();
    support::publish_executable(&fake_tilt);

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();

    assert_eq!(model.overall_status(), OverallStatus::Stopped);
    assert!(model.services.is_empty());
    assert_eq!(model.warning(), None);
}

#[test]
fn dashboard_rejects_a_tilt_api_for_another_project() {
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
if [ "$2" = "sessions" ]; then
  printf '%s\n' '{"items":[{"spec":{"tiltfilePath":"/another/project/Tiltfile"},"status":{"pid":999,"startTime":"2026-08-20T01:02:03Z"}}]}'
else
  printf '%s\n' '{"items":[{"metadata":{"name":"wrong-project"},"status":{"updateStatus":"ok","runtimeStatus":"ok"}}]}'
fi
"#,
    )
    .unwrap();
    support::publish_executable(&fake_tilt);

    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    let error = model.refresh_with_tilt(&fake_tilt).unwrap_err();

    assert!(error.to_string().contains("another Tiltfile"));
    assert!(model.services.is_empty());
    assert_eq!(model.overall_status(), OverallStatus::Failed);
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
    support::publish_executable(&fake_herdr);

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
    assert!(!model.can_start());
}

#[test]
fn dashboard_triggers_a_resource_on_the_active_tilt_port() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.canonicalize().unwrap()),
    };
    let session_path = record_path(state.path(), project.tiltfile.as_ref().unwrap());
    fs::create_dir_all(session_path.parent().unwrap()).unwrap();
    fs::write(
        session_path,
        serde_json::to_vec(&SessionRecord {
            tiltfile: project.tiltfile.clone().unwrap(),
            project_root: project.root.clone(),
            port: 41234,
            runner_pid: 320,
            tilt_pid: 321,
            started_unix_ms: 1234,
            phase: SessionPhase::Running,
            exit_code: None,
        })
        .unwrap(),
    )
    .unwrap();
    let capture = workspace.path().join("tilt-args");
    let fake_tilt = workspace.path().join("tilt");
    fs::write(
        &fake_tilt,
        format!(
            r#"#!/bin/sh
if [ "$1" = "trigger" ]; then
  printf '%s\n' "$@" > '{}'
elif [ "$2" = "sessions" ]; then
  printf '%s\n' '{{"items":[{{"spec":{{"tiltfilePath":"{}"}},"status":{{"pid":321,"startTime":"2026-08-20T01:02:03Z"}}}}]}}'
else
  printf '%s\n' '{{"items":[{{"metadata":{{"name":"api"}},"status":{{"order":1,"updateStatus":"ok","runtimeStatus":"ok"}}}}]}}'
fi
"#,
            capture.display(),
            project.tiltfile.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    support::publish_executable(&fake_tilt);
    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();

    model.trigger_service_with_tilt(&fake_tilt, "api").unwrap();

    assert_eq!(
        fs::read_to_string(capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["trigger", "api", "--port", "41234"]
    );
}

#[test]
fn dashboard_toggles_resources_between_enabled_and_disabled() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tiltfile = workspace.path().join("Tiltfile");
    fs::write(&tiltfile, "").unwrap();
    let project = Project {
        root: workspace.path().canonicalize().unwrap(),
        tiltfile: Some(tiltfile.canonicalize().unwrap()),
    };
    let capture = workspace.path().join("tilt-calls");
    let fake_tilt = workspace.path().join("tilt");
    fs::write(
        &fake_tilt,
        format!(
            r#"#!/bin/sh
if [ "$1" = "enable" ] || [ "$1" = "disable" ]; then
  printf '%s\n' "$*" >> '{}'
elif [ "$2" = "sessions" ]; then
  printf '%s\n' '{{"items":[{{"spec":{{"tiltfilePath":"{}"}},"status":{{"pid":321,"startTime":"2026-08-20T01:02:03Z"}}}}]}}'
else
  printf '%s\n' '{{"items":[
    {{"metadata":{{"name":"api"}},"status":{{"order":1,"updateStatus":"ok","runtimeStatus":"ok"}}}},
    {{"metadata":{{"name":"worker"}},"status":{{"order":2,"disableStatus":{{"state":"Disabled"}}}}}}
  ]}}'
fi
"#,
            capture.display(),
            project.tiltfile.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    support::publish_executable(&fake_tilt);
    let mut model = DashboardModel::new(project, state.path().to_path_buf());
    model.refresh_with_tilt(&fake_tilt).unwrap();
    let api = model.services[0].clone();
    let worker = model.services[1].clone();

    model.toggle_service_with_tilt(&fake_tilt, &api).unwrap();
    model.toggle_service_with_tilt(&fake_tilt, &worker).unwrap();

    assert_eq!(
        fs::read_to_string(capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["disable api --port 10350", "enable worker --port 10350"]
    );
}
