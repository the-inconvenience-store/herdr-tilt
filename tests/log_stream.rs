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
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'build started\\nserver ready\\n'\n",
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
        ["logs", "api", "--follow", "--port", "41234"]
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
