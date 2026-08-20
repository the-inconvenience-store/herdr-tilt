#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use herdr_tilt::logs::{LogBuffer, TiltLogStream};

#[test]
fn tilt_log_stream_merges_build_and_application_output() {
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("args");
    let fake_tilt = temp.path().join("tilt");
    fs::write(
        &fake_tilt,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}-'\"$5\"\nif [ \"$5\" = \"build\" ]; then printf 'build started\\n'; else printf 'server ready\\n'; fi\n",
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

    let lines = logs.lines().collect::<Vec<_>>();
    assert!(
        lines.contains(&"tilt │ build started"),
        "missing Tilt output: {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| **line == "app │ server ready")
            .count(),
        1,
        "missing application output: {lines:?}"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("args-build"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "logs", "api", "--follow", "--source", "build", "--port", "41234"
        ]
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("args-runtime"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "logs", "api", "--follow", "--source", "runtime", "--port", "41234"
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

    assert_eq!(logs.lines().count(), 2);
    assert!(logs.lines().all(|line| line.len() == 8 * 1024));
    assert_eq!(logs.truncated_lines(), 2);
}
