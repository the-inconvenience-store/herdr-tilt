#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use herdr_tilt::logs::{LogBuffer, TiltLogStream};

#[test]
fn tilt_log_stream_uses_the_resource_and_port_and_delivers_output() {
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("args");
    let fake_tilt = temp.path().join("tilt");
    fs::write(
        &fake_tilt,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"get\" ]; then exit 1; fi\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'build started\\nserver ready\\n'\n",
            capture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_tilt, fs::Permissions::from_mode(0o755)).unwrap();
    let mut stream = TiltLogStream::spawn(&fake_tilt, "api", 41234).unwrap();
    let mut logs = LogBuffer::with_limits(10, 80);
    let deadline = Instant::now() + Duration::from_secs(2);

    while logs.len() < 2 && Instant::now() < deadline {
        stream.poll_into(&mut logs, 32);
        thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        logs.lines().collect::<Vec<_>>(),
        ["build started", "server ready"]
    );
    assert_eq!(
        fs::read_to_string(capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "logs", "api", "--follow", "--source", "all", "--port", "41234"
        ]
    );
}

#[test]
fn kubernetes_resources_merge_build_and_container_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let tilt_capture = temp.path().join("tilt-args");
    let kubectl_capture = temp.path().join("kubectl-args");
    let fake_tilt = temp.path().join("tilt");
    let fake_kubectl = temp.path().join("kubectl");
    fs::write(
        &fake_tilt,
        format!(
            r#"#!/bin/sh
if [ "$2" = "kubernetesdiscovery" ]; then
  printf '%s\n' '{{"spec":{{"cluster":"default"}},"status":{{"pods":[{{"name":"api-123","namespace":"apps"}}]}}}}'
elif [ "$2" = "cluster" ]; then
  printf '%s\n' '{{"spec":{{"connection":{{"kubernetes":{{"context":"kind-dev"}}}}}}}}'
else
  printf '%s\n' "$@" > '{}'
  printf 'image built\n'
fi
"#,
            tilt_capture.display()
        ),
    )
    .unwrap();
    fs::write(
        &fake_kubectl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'api-123/api hello from stdout\\n'\n",
            kubectl_capture.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_tilt, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&fake_kubectl, fs::Permissions::from_mode(0o755)).unwrap();
    let mut stream =
        TiltLogStream::spawn_with_kubectl(&fake_tilt, &fake_kubectl, "api", 41234).unwrap();
    let mut logs = LogBuffer::with_limits(10, 200);
    let deadline = Instant::now() + Duration::from_secs(2);

    while logs.len() < 2 && Instant::now() < deadline {
        stream.poll_into(&mut logs, 32);
        thread::sleep(Duration::from_millis(10));
    }

    let lines = logs.lines().collect::<Vec<_>>();
    assert_eq!(stream.kubernetes_stream_count(), 1);
    assert!(lines.contains(&"image built"));
    assert!(
        lines.contains(&"k8s │ api-123/api hello from stdout"),
        "missing Kubernetes output: {lines:?}"
    );
    assert_eq!(
        fs::read_to_string(tilt_capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "logs", "api", "--follow", "--source", "build", "--port", "41234"
        ]
    );
    assert_eq!(
        fs::read_to_string(kubectl_capture)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--context=kind-dev",
            "logs",
            "pod/api-123",
            "--namespace=apps",
            "--all-containers=true",
            "--prefix=true",
            "--follow",
            "--tail=200",
            "--ignore-errors=true"
        ]
    );
}

#[test]
fn tilt_log_stream_bounds_a_single_unterminated_record_while_reading() {
    let temp = tempfile::tempdir().unwrap();
    let fake_tilt = temp.path().join("tilt");
    fs::write(
        &fake_tilt,
        "#!/bin/sh\ni=0\nwhile [ $i -lt 20000 ]; do printf x; i=$((i + 1)); done\nprintf '\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&fake_tilt, fs::Permissions::from_mode(0o755)).unwrap();
    let mut stream = TiltLogStream::spawn(&fake_tilt, "api", 10350).unwrap();
    let mut logs = LogBuffer::with_limits(10, 20_000);
    let deadline = Instant::now() + Duration::from_secs(2);

    while logs.is_empty() && Instant::now() < deadline {
        stream.poll_into(&mut logs, 32);
        thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(logs.lines().next().unwrap().len(), 8 * 1024);
    assert_eq!(logs.truncated_lines(), 1);
}
