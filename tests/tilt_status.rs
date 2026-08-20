use std::path::Path;

use herdr_tilt::tilt::{CircleStatus, parse_session_identity, parse_ui_resources};

#[test]
fn ui_resources_become_ordered_four_color_services() {
    let json = r#"
    {
      "items": [
        {
          "metadata": {"name": "worker"},
          "status": {"order": 2, "updateStatus": "error", "runtimeStatus": "ok"}
        },
        {
          "metadata": {"name": "frontend"},
          "status": {"order": 1, "updateStatus": "ok", "runtimeStatus": "ok"}
        },
        {
          "metadata": {"name": "assets"},
          "status": {"order": 3, "updateStatus": "in_progress", "runtimeStatus": "pending"}
        },
        {
          "metadata": {"name": "optional"},
          "status": {
            "order": 4,
            "updateStatus": "none",
            "runtimeStatus": "none",
            "disableStatus": {"state": "Disabled"}
          }
        },
        {
          "metadata": {"name": "(Tiltfile)"},
          "status": {
            "order": 0,
            "updateStatus": "error",
            "buildHistory": [{"error": "Tiltfile: unknown function"}]
          }
        }
      ]
    }
    "#;

    let snapshot = parse_ui_resources(json).unwrap();

    assert_eq!(
        snapshot
            .services
            .iter()
            .map(|service| (service.name.as_str(), service.status))
            .collect::<Vec<_>>(),
        [
            ("frontend", CircleStatus::Green),
            ("worker", CircleStatus::Red),
            ("assets", CircleStatus::Orange),
            ("optional", CircleStatus::Grey),
        ]
    );
    assert_eq!(
        snapshot.tiltfile_error.as_deref(),
        Some("Tiltfile: unknown function")
    );
}

#[test]
fn session_identity_uses_the_tilt_reported_tiltfile_and_pid() {
    let json = r#"
    {
      "items": [{
        "spec": {"tiltfilePath": "/work/project/Tiltfile"},
        "status": {"pid": 4321, "startTime": "2026-08-20T01:02:03Z"}
      }]
    }
    "#;

    let identity = parse_session_identity(json).unwrap();

    assert_eq!(identity.tiltfile, Path::new("/work/project/Tiltfile"));
    assert_eq!(identity.pid, 4321);
    assert_eq!(identity.start_time, "2026-08-20T01:02:03Z");
}
